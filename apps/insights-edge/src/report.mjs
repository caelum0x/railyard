// Weekly funnel report: every site in the SITES KV, last 7 full UTC days
// against the 7 before, emailed through Scribase Mail (Rally Send protocol).
// Runs from the Worker cron (Mondays 07:00 UTC -- wrangler.jsonc's "triggers.crons",
// fixed 2026-10-26 to "0 7 * * MON" after it silently fired every Sunday for weeks;
// see the comment there) and from POST /api/report.

import { siteStats } from "./store.mjs";

const DAY = 86_400_000;

export async function listSites(env) {
  const out = [];
  let cursor;
  do {
    const page = await env.SITES.list({ prefix: "site:", cursor });
    for (const k of page.keys) {
      // Self-serve entries carry list metadata, so the weekly run never reads them one by one.
      if (k.metadata?.self_serve) {
        out.push({ key: k.name.slice(5), label: k.name.slice(5), self_serve: true });
        continue;
      }
      const raw = await env.SITES.get(k.name);
      if (!raw) continue;
      const s = JSON.parse(raw);
      out.push({ key: k.name.slice(5), label: s.label ?? s.app_id ?? k.name.slice(5), ...s });
    }
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  return out.sort((a, b) => a.label.localeCompare(b.label));
}

/** Window ends at today's 00:00 UTC. */
export function windows(nowMs, days = 7) {
  const end = Math.floor(nowMs / DAY) * DAY;
  const iso = (ms) => new Date(ms).toISOString();
  return { cur: [iso(end - days * DAY), iso(end)], prev: [iso(end - 2 * days * DAY), iso(end - days * DAY)] };
}

export async function buildReport(env, nowMs, days = 7) {
  const all = await listSites(env);
  // Self-serve sites belong to their owners: counted here, never itemised in our report.
  const sites = all.filter((s) => !s.self_serve);
  const w = windows(nowMs, days);
  const rows = [];
  for (const s of sites) {
    const cur = await siteStats(env.DB, s.key, ...w.cur);
    const prev = await siteStats(env.DB, s.key, ...w.prev);
    rows.push({ site: s.label, cur, prev });
  }
  return { from: w.cur[0].slice(0, 10), to: w.cur[1].slice(0, 10), rows, self_serve_sites: all.length - sites.length };
}

const esc = (v) => String(v).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const delta = (a, b) => (b === 0 ? (a === 0 ? "" : " (new)") : ` (${a >= b ? "+" : ""}${Math.round(((a - b) / b) * 100)}%)`);
const pct = (a, b) => (b ? `${Math.round((a / b) * 100)}%` : "-");

export function renderReport(r) {
  const total = r.rows.reduce((acc, x) => ({ v: acc.v + x.cur.visitors, p: acc.p + x.cur.pageviews }), { v: 0, p: 0 });
  const subject = `Railyard Insights week ${r.from}..${r.to}: ${total.v} visitors, ${total.p} page views`;
  const text = [subject, ""];
  let html = `<div style="font-family:system-ui,sans-serif;font-size:14px;color:#111"><h2 style="font-size:18px">${esc(subject)}</h2>`;
  html += `<table cellpadding="6" style="border-collapse:collapse;border:1px solid #ddd"><tr style="background:#f4f4f4"><th align="left">Site</th><th>Visitors</th><th>2+ pages</th><th>Custom event</th><th>Page views</th></tr>`;
  for (const { site, cur, prev } of r.rows) {
    html += `<tr><td>${esc(site)}</td><td align="right">${cur.visitors}${esc(delta(cur.visitors, prev.visitors))}</td><td align="right">${cur.engaged} (${pct(cur.engaged, cur.visitors)})</td><td align="right">${cur.converted} (${pct(cur.converted, cur.visitors)})</td><td align="right">${cur.pageviews}${esc(delta(cur.pageviews, prev.pageviews))}</td></tr>`;
    text.push(`${site}: ${cur.visitors} visitors${delta(cur.visitors, prev.visitors)} -> ${cur.engaged} viewed 2+ pages -> ${cur.converted} fired a custom event; ${cur.pageviews} page views`);
  }
  html += "</table>";
  for (const { site, cur } of r.rows) {
    if (!cur.pageviews && !cur.events.length) continue;
    html += `<h3 style="font-size:15px;margin-top:20px">${esc(site)}</h3>`;
    const list = (title, items, fmt) => (items.length ? `<p><b>${title}:</b> ${items.map(fmt).join(", ")}</p>` : "");
    html += list("Top pages", cur.top_pages, (x) => `${esc(x.url)} (${x.n})`);
    html += list("Top referrers", cur.top_referrers, (x) => `${esc(x.host)} (${x.n})`);
    html += list("Events", cur.events, (x) => `${esc(x.name)} (${x.n})`);
    html += list("UTM sources", cur.utm_sources ?? [], (x) => `${esc(x.source)} (${x.n})`);
    html += list("Agent failures", cur.agent_failures ?? [], (x) => `${esc(x.agent)} ${esc(x.model || "none")} ${esc(x.error)}`);
    text.push("", site, ...cur.top_pages.map((x) => `  page ${x.url} ${x.n}`), ...cur.top_referrers.map((x) => `  from ${x.host} ${x.n}`), ...cur.events.map((x) => `  event ${x.name} ${x.n}`), ...(cur.utm_sources ?? []).map((x) => `  utm ${x.source} ${x.n}`), ...(cur.agent_failures ?? []).map((x) => `  agent failure ${x.agent} ${x.model || "none"} ${x.error}`));
  }
  html += `<p><b>Self-serve sites signed up:</b> ${r.self_serve_sites ?? 0}</p>`;
  text.push("", `Self-serve sites signed up: ${r.self_serve_sites ?? 0}`);
  html += `<p style="color:#666;font-size:12px">Cookieless: visitors are a daily salted hash; no IP or cookie is stored. Visitors are counted per day.</p></div>`;
  return { subject, html, text: text.join("\n") };
}

export async function sendReport(env, nowMs, fetcher = fetch) {
  if (!env.RALLY_TX_URL || !env.RALLY_TX_KEY || !env.REPORT_TO) return { sent: false, reason: "report mail not configured" };
  const report = await buildReport(env, nowMs);
  const { subject, html, text } = renderReport(report);
  const res = await fetcher(env.RALLY_TX_URL, {
    method: "POST",
    headers: { authorization: `Bearer ${env.RALLY_TX_KEY}`, "content-type": "application/json" },
    body: JSON.stringify({
      product: env.REPORT_PRODUCT ?? "menivor",
      project: "railyard-insights",
      to: env.REPORT_TO,
      from_local: env.REPORT_FROM_LOCAL ?? "no-reply",
      from_name: "Railyard Insights",
      subject,
      html,
      text,
      idempotency_key: `railyard-insights-weekly-${report.to}-${new Date(nowMs).toISOString().slice(0, 13)}`,
      tags: ["railyard-insights-weekly"],
    }),
  });
  const detail = (await res.text()).slice(0, 300);
  return { sent: res.status === 202, status: res.status, detail, subject };
}
