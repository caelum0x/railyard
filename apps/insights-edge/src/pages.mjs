// Public HTML for Railyard Insights: the landing page with the sign-up form
// (works without JavaScript), the private per-site dashboard, and the privacy
// page. Every value from a person or a URL is escaped.

import { FREE_EVENTS_PER_MONTH, snippetFor } from "./selfserve.mjs";

export const CANONICAL = "https://insights.ereboslab.com";
const CONTACT = "caelum0x42@gmail.com";

export const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const n = (v) => Number(v ?? 0).toLocaleString("en-US");
const FREE = n(FREE_EVENTS_PER_MONTH);

const CSS = `*{box-sizing:border-box}body{margin:0;font:16px/1.55 system-ui,-apple-system,Segoe UI,sans-serif;color:#111;background:#fafafa}
main{max-width:760px;margin:0 auto;padding:40px 20px 64px}h1{font-size:34px;line-height:1.15;margin:0 0 12px}h2{font-size:20px;margin:36px 0 10px}
p{margin:0 0 12px}.muted{color:#555}.small{font-size:13px;color:#666}a{color:#0b57d0}
form.signup{display:grid;gap:10px;background:#fff;border:1px solid #e3e3e3;border-radius:12px;padding:18px;margin:22px 0}
input{font:inherit;padding:11px 12px;border:1px solid #ccc;border-radius:8px;width:100%}button{font:inherit;font-weight:600;padding:12px;border:0;border-radius:8px;background:#111;color:#fff;cursor:pointer}
button.danger{background:#b3261e}.error{background:#fdecea;color:#8a1c14;padding:10px 12px;border-radius:8px}.ok{background:#e8f5e9;color:#1b5e20;padding:10px 12px;border-radius:8px}
pre{background:#111;color:#f5f5f5;padding:12px;border-radius:8px;white-space:pre-wrap;word-break:break-all;font-size:13px}
ul.points{padding-left:20px}ul.points li{margin:6px 0}.grid{display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin:16px 0}
.stat{background:#fff;border:1px solid #e3e3e3;border-radius:10px;padding:12px}.stat b{display:block;font-size:24px}
table{width:100%;border-collapse:collapse;background:#fff;border:1px solid #e3e3e3;border-radius:10px;font-size:14px}td,th{padding:8px 10px;border-bottom:1px solid #eee;text-align:left}td.r,th.r{text-align:right}
nav.days a{margin-right:12px}footer{margin-top:48px;font-size:13px;color:#666}@media(max-width:600px){.grid{grid-template-columns:repeat(2,1fr)}h1{font-size:28px}}`;

function shell({ title, description, body, canonical, tag, robots = "index,follow", head = "" }) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(title)}</title><meta name="description" content="${esc(description)}"><meta name="robots" content="${robots}">
${canonical ? `<link rel="canonical" href="${esc(canonical)}"><meta property="og:url" content="${esc(canonical)}">` : ""}
<meta property="og:title" content="${esc(title)}"><meta property="og:description" content="${esc(description)}"><meta property="og:type" content="website">
<style>${CSS}</style>${head}</head><body><main>${body}
<footer><a href="/">Railyard Insights</a> &middot; <a href="/privacy">Privacy</a> &middot; Made by <a href="https://ereboslab.com/">Erebos Lab</a> &middot; ${esc(CONTACT)}</footer></main>
${tag ? `<script defer src="/s.js" data-site="${esc(tag)}"></script>` : ""}</body></html>`;
}

export function landingPage({ error, values = {}, tag } = {}) {
  const ld = JSON.stringify({
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    name: "Railyard Insights",
    applicationCategory: "BusinessApplication",
    operatingSystem: "Web",
    url: `${CANONICAL}/`,
    description: "Cookieless website analytics: one script tag, no cookies, no IP addresses stored.",
    offers: { "@type": "Offer", price: "0", priceCurrency: "USD", description: `Up to ${FREE} stored events a month per site` },
  });
  const body = `<h1>Website analytics without cookies</h1>
<p class="muted">Add one script tag and see visitors, top pages, referrers, UTM campaigns, custom events and page speed. The tag sets no cookies, uses no local storage, and no IP address is ever stored. Free for up to ${FREE} events a month per site.</p>
<form class="signup" method="post" action="/api/sites">
${error ? `<p class="error" role="alert">${esc(error)}</p>` : ""}
<label>Your site's domain<input name="domain" placeholder="shop.example.com" required autocomplete="url" value="${esc(values.domain)}"></label>
<label>Your email (we send the dashboard link here)<input type="email" name="email" placeholder="you@company.com" required autocomplete="email" value="${esc(values.email)}"></label>
<button type="submit">Get my tag and dashboard</button>
<p class="small">No password and no card. You get the tag and a private dashboard link on the next page.</p>
</form>
<h2>What you get</h2>
<ul class="points">
<li><b>Visitors and page views</b> for the last 7, 30 or 90 days, with how many viewed 2+ pages.</li>
<li><b>Where they came from:</b> top referrers and <code>utm_source</code> per campaign link.</li>
<li><b>Custom events:</b> call <code>railyard.track("signup")</code> and see the count next to your visitors.</li>
<li><b>Page speed</b> from real visits: LCP, INP and CLS are sent once per page view.</li>
</ul>
<h2>How it stays cookieless</h2>
<ul class="points">
<li>A visitor is a hash of IP address, browser and a secret that changes every UTC day. The IP is used for that hash in memory and then dropped; it is never written anywhere.</li>
<li>No cookies, no local storage, no fingerprinting across days or sites. A returning visitor counts again the next day.</li>
<li>Browsers with Do Not Track on send nothing. Bots, crawlers, link previews and prefetches are dropped.</li>
<li>Events are stored only from your own domain and its subdomains.</li>
<li>Whether you still need a consent banner depends on your country's rules; the tag itself reads and writes nothing on the visitor's device.</li>
</ul>
<h2>Limits</h2>
<p>Free: up to ${FREE} stored events a month per site. Past that, new events are counted but not stored until the 1st of next month; nothing already stored is removed. You can delete a site and all of its data from its dashboard at any time.</p>
<script type="application/ld+json">${ld.replace(/</g, "\\u003c")}</script>`;
  return shell({ title: "Free cookieless website analytics, one script tag | Railyard Insights", description: `Privacy-friendly website analytics with no cookies and no stored IP addresses. Visitors, pages, referrers, UTM sources, events and page speed. Free up to ${FREE} events a month.`, body, canonical: `${CANONICAL}/`, tag });
}

export function privacyPage({ tag } = {}) {
  const body = `<h1>Privacy</h1>
<p class="muted">Railyard Insights is run by Erebos Lab. Last updated 2026-10-12.</p>
<h2>Visitors of sites that use the tag</h2>
<p>The tag sends the page address, the referring page, the page title, the browser language, the screen size, the country (from the network, not GPS), page speed numbers, and any custom event name and fields the site sends. It sets no cookies and uses no local storage. Your IP address and browser string are combined with a secret that changes every day into a one-way hash used to count unique visitors for that day; the IP address itself is never stored or logged. Browsers with Do Not Track on send nothing.</p>
<h2>People who add a site</h2>
<p>We store your email address and your domain to send you the dashboard link. We do not use your email for marketing. The dashboard link is a secret; we keep only a hash of it.</p>
<h2>Where it is stored</h2>
<p>On Cloudflare (Workers, D1 and KV). The link email is sent through Scribase Mail.</p>
<h2>Deleting your data</h2>
<p>Open your dashboard link and use <b>Delete this site</b>: the site, its sign-up record, its dashboard link and every stored event are removed at once. Lost the link? Email ${esc(CONTACT)} from the address you signed up with and we delete it by hand.</p>`;
  return shell({ title: "Privacy | Railyard Insights", description: "What Railyard Insights stores, what it never stores, and how to delete your data.", body, canonical: `${CANONICAL}/privacy`, tag });
}

const list = (title, rows, cols) =>
  `<h2>${esc(title)}</h2>${rows.length ? `<table><tr>${cols.map((c) => `<th${c.r ? ' class="r"' : ""}>${esc(c.h)}</th>`).join("")}</tr>${rows.map((r) => `<tr>${cols.map((c) => `<td${c.r ? ' class="r"' : ""}>${esc(c.v(r))}</td>`).join("")}</tr>`).join("")}</table>` : `<p class="small">Nothing yet.</p>`}`;

export function dashboardPage({ token, origin, key, site, stats, days, usage, fresh, capLine }) {
  const cur = stats;
  const snippet = snippetFor(origin, key);
  const waiting = usage.all === 0;
  const body = `<h1>${esc(site.label)}</h1>
${fresh ? `<p class="ok">Your site is added. Bookmark this page: it is your private dashboard${fresh === "mailed" ? ", and we emailed you the link" : ""}.</p>` : ""}
${waiting ? `<h2>Step 1: add the tag</h2><p>Paste this into the <code>&lt;head&gt;</code> of every page of ${esc(site.label)}, deploy, then open your site in a browser. Your first page view shows up here within a few seconds of a reload.</p><pre>${esc(snippet)}</pre>` : ""}
<nav class="days">Last: ${[7, 30, 90].map((d) => (d === days ? `<b>${d} days</b>` : `<a href="/d/${esc(token)}?days=${d}">${d} days</a>`)).join(" ")}</nav>
<div class="grid">
<div class="stat"><b>${n(cur.visitors)}</b>visitors</div><div class="stat"><b>${n(cur.pageviews)}</b>page views</div>
<div class="stat"><b>${n(cur.engaged)}</b>viewed 2+ pages</div><div class="stat"><b>${n(cur.converted)}</b>sent a custom event</div>
</div>
<p class="small">${n(usage.month)} of ${n(site.events_limit)} stored events used this month.${capLine ? ` ${esc(capLine)}` : ""}</p>
${list("Top pages", cur.top_pages, [{ h: "Page", v: (r) => r.url }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("Top referrers", cur.top_referrers, [{ h: "Site", v: (r) => r.host }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("UTM sources", cur.utm_sources, [{ h: "utm_source", v: (r) => r.source }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("Custom events", cur.events, [{ h: "Event", v: (r) => r.name }, { h: "Count", v: (r) => n(r.n), r: 1 }])}
${waiting ? "" : `<h2>Your tag</h2><pre>${esc(snippet)}</pre>`}
<p class="small">Count a sign-up or purchase with <code>railyard.track("signup")</code> after the tag loads. Visitors are counted per day, without cookies.</p>
<h2>Delete this site</h2>
<form method="post" action="/d/${esc(token)}/delete" style="display:grid;gap:10px">
<label>Type <b>${esc(site.label)}</b> to delete the site and every stored event. This cannot be undone.<input name="confirm" required autocomplete="off"></label>
<button class="danger" type="submit">Delete this site and its data</button></form>`;
  return shell({ title: `${site.label} analytics | Railyard Insights`, description: "Private dashboard", body, robots: "noindex,nofollow" });
}

export function messagePage(title, text, status = "") {
  return shell({ title: `${title} | Railyard Insights`, description: title, robots: "noindex", body: `<h1>${esc(title)}</h1>${status ? `<p class="${status}">${esc(text)}</p>` : `<p>${esc(text)}</p>`}<p><a href="/">Back to Railyard Insights</a></p>` });
}

export const ROBOTS = `User-agent: *\nAllow: /\nDisallow: /d/\nDisallow: /api/\nSitemap: ${CANONICAL}/sitemap.xml\n`;
export const SITEMAP = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>${CANONICAL}/</loc><lastmod>2026-10-12</lastmod></url><url><loc>${CANONICAL}/privacy</loc><lastmod>2026-10-12</lastmod></url></urlset>\n`;
