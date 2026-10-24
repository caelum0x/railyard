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

export function shell({ title, description, body, canonical, tag, robots = "index,follow", head = "" }) {
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
<p>The dashboard also lists agent failures on their own: the agent, the model, and the error name. The prompt is not stored. <a href="/live-demo">The live example</a> shows that list.</p>
<p><a href="/live-demo">See a live example with real numbers, before you sign up &rarr;</a></p>
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
<h2>Read more</h2>
<ul class="points">
<li><a href="/live-demo">See a live example dashboard</a> with this site's own real traffic, not a mockup.</li>
<li><a href="/cookieless-analytics">What "cookieless analytics" means</a>, and exactly how this tag counts a visitor without one.</li>
<li><a href="/google-analytics-alternative">A privacy-friendly Google Analytics alternative</a>.</li>
<li><a href="/plausible-alternative">A Plausible alternative</a> with a free tier that's not a trial.</li>
<li><a href="/fathom-analytics-alternative">A Fathom Analytics alternative</a> with no card required to start.</li>
</ul>
<script type="application/ld+json">${ld.replace(/</g, "\\u003c")}</script>`;
  return shell({ title: "Free cookieless website analytics, one script tag | Railyard Insights", description: `Privacy-friendly website analytics with no cookies and no stored IP addresses. Visitors, pages, referrers, UTM sources, events and page speed. Free up to ${FREE} events a month.`, body, canonical: `${CANONICAL}/`, tag });
}

export function privacyPage({ tag } = {}) {
  const body = `<h1>Privacy</h1>
<p class="muted">Railyard Insights is run by Erebos Lab. Last updated 2026-10-22.</p>
<h2>Visitors of sites that use the tag</h2>
<p>The tag sends the page address, the referring page, the page title, the browser language, the screen size, the country (from the network, not GPS), page speed numbers, and any custom event name and fields the site sends. An agent failure stores only the agent name, the model name, and the error name. A prompt or a key sent with that event is dropped and not stored. It sets no cookies and uses no local storage. Your IP address and browser string are combined with a secret that changes every day into a one-way hash used to count unique visitors for that day; the IP address itself is never stored or logged. Browsers with Do Not Track on send nothing.</p>
<h2>People who add a site</h2>
<p>We store your email address and your domain to send you the dashboard link. We do not use your email for marketing. The dashboard link is a secret; we keep only a hash of it.</p>
<h2>Where it is stored</h2>
<p>On Cloudflare (Workers, D1 and KV). The link email is sent through Scribase Mail.</p>
<h2>This website</h2>
<p>The public pages of insights.ereboslab.com count their own visits with this same tag and with Cloudflare Web Analytics, which also sets no cookies. Private dashboards load neither.</p>
<h2>Deleting your data</h2>
<p>Open your dashboard link and use <b>Delete this site</b>: the site, its sign-up record, its dashboard link and every stored event are removed at once. Lost the link? Email ${esc(CONTACT)} from the address you signed up with and we delete it by hand.</p>`;
  return shell({ title: "Privacy | Railyard Insights", description: "What Railyard Insights stores, what it never stores, and how to delete your data.", body, canonical: `${CANONICAL}/privacy`, tag });
}

// SEO landing pages (2026-10-14, named comparisons added 2026-10-15): each
// targets one real search query with only true, checkable statements.
// `cookielessAnalyticsPage` and `googleAnalyticsAlternativePage` describe only
// what Railyard Insights itself does, with no named competitor. The two named
// comparison pages below (`plausibleAlternativePage`, `fathomAlternativePage`)
// name a specific competitor factually, matching the organic "alternative to"
// pages already shipped elsewhere in this portfolio (vcpeer.com's
// /wetransfer-alternative): every number about that competitor is taken from
// its own live public pricing page on the date noted, cited inline, and never
// used in a paid ad.
const seoFooter = `<h2>Try it on your site</h2>
<form class="signup" method="post" action="/api/sites">
<label>Your site's domain<input name="domain" placeholder="shop.example.com" required autocomplete="url"></label>
<label>Your email (we send the dashboard link here)<input type="email" name="email" placeholder="you@company.com" required autocomplete="email"></label>
<button type="submit">Get my tag and dashboard</button>
<p class="small">No password and no card. Free for up to ${FREE} stored events a month per site.</p>
</form>`;

export function cookielessAnalyticsPage({ tag } = {}) {
  const body = `<h1>Cookieless website analytics</h1>
<p class="muted">"Cookieless analytics" means a visitor is counted without a cookie, without local storage and without any identifier that follows them from one day or one site to the next. Railyard Insights is built that way end to end.</p>
<h2>How Railyard Insights counts a visitor without a cookie</h2>
<ul class="points">
<li>A visitor is a one-way hash of their IP address, browser string and a secret that changes every UTC day. The hash is computed in memory for that one request and the IP address itself is never written anywhere.</li>
<li>Nothing is stored in the browser: no cookie, no <code>localStorage</code>, no fingerprinting script that tries to re-identify the same device across days.</li>
<li>Because the salt rotates daily, the same visitor on two different days produces two different hashes; there is no way to link them back to one person over time.</li>
<li>A browser sending Do Not Track is not counted at all; bots, crawlers, link-preview fetchers and prefetch requests are dropped before anything is stored.</li>
</ul>
<h2>Why this matters</h2>
<p>A cookie-based analytics tool stores a unique id in the visitor's browser, so it can recognize the same person on their next visit. That id is personal data under most privacy law, which is why cookie-based analytics usually needs a consent banner. A script that never sets that id, and never stores the raw IP address either, counts unique visitors per day without collecting the personal data a banner exists to ask permission for.</p>
<h2>What you get</h2>
<ul class="points">
<li>Visitors and page views over the last 7, 30 or 90 days.</li>
<li>Top pages and top referrers, with <code>utm_source</code> per campaign link.</li>
<li>Custom events: call <code>railyard.track("signup")</code> and see the count.</li>
<li>Real-visitor page speed: LCP, INP and CLS at the 75th percentile.</li>
</ul>
<p>One script tag, a private dashboard link mailed to you, free for up to ${FREE} stored events a month. <a href="/privacy">Read exactly what is and is not stored</a>.</p>
${seoFooter}`;
  return shell({
    title: "Cookieless website analytics, explained | Railyard Insights",
    description: "What cookieless analytics means and how Railyard Insights counts visitors without cookies, local storage or stored IP addresses. Free up to " + FREE + " events a month.",
    body,
    canonical: `${CANONICAL}/cookieless-analytics`,
    tag,
  });
}

export function googleAnalyticsAlternativePage({ tag } = {}) {
  const body = `<h1>A privacy-friendly Google Analytics alternative</h1>
<p class="muted">Teams move off a cookie-based analytics setup for a few recurring reasons: the consent banner it requires, the size of the script, and wanting one plain dashboard instead of a reporting suite built for ad platforms. Railyard Insights is built around the opposite defaults.</p>
<h2>What "privacy-friendly" means here, specifically</h2>
<ul class="points">
<li>No cookie and no <code>localStorage</code> entry is ever set by the tag, so there is nothing on the visitor's device for a cookie-consent banner to ask permission for.</li>
<li>No IP address is stored. It is combined in memory with a secret that changes every day into a one-way hash used only to count that day's unique visitors, then dropped.</li>
<li>The tracker script is small (under 3&nbsp;KB gzipped) and loads with <code>defer</code>, so it does not block the page.</li>
<li>One dashboard: visitors, page views, top pages, top referrers, <code>utm_source</code> campaigns, custom events and real-visitor page speed (LCP, INP, CLS) — no separate reporting tool needed for the numbers most sites check day to day.</li>
</ul>
<h2>Moving over</h2>
<p>Add one script tag with your site's key, keep your current analytics running alongside it for as long as you like, and compare the numbers yourself. There is no account migration: a new site gets a dashboard the moment you sign up, free for up to ${FREE} stored events a month.</p>
<p><a href="/privacy">Full detail on what is stored and what never is</a>.</p>
${seoFooter}`;
  return shell({
    title: "Privacy-friendly Google Analytics alternative | Railyard Insights",
    description: "A cookieless, no-stored-IP alternative to a cookie-based analytics setup: one script tag, one dashboard, free up to " + FREE + " events a month.",
    body,
    canonical: `${CANONICAL}/google-analytics-alternative`,
    tag,
  });
}

export function plausibleAlternativePage({ tag } = {}) {
  const body = `<h1>A Plausible alternative with a real free tier</h1>
<p class="muted">Plausible Analytics is a well-known cookieless, privacy-first analytics tool, and a fair bar to compare against. Checked on Plausible's own pricing page: its <b>Starter plan is $9/month for up to 10,000 monthly pageviews on one site</b>, <b>Growth is $14/month for up to 3 sites</b>, and <b>Business is $19/month for up to 10 sites</b> — every paid plan starts as a 30-day free trial, no card required up front. Railyard Insights answers the same search with a free tier that is not a trial: <b>free for up to ${FREE} stored events a month per site</b>, no credit card at sign-up, for as long as you use it.</p>
<h2>Side by side</h2>
<table><tr><th></th><th>Plausible (Starter)</th><th>Railyard Insights (free)</th></tr>
<tr><td>Price</td><td>$9/month, billed</td><td>$0, no card</td></tr>
<tr><td>Monthly cap on that plan</td><td>10,000 pageviews</td><td>${FREE} stored events</td></tr>
<tr><td>How you get in</td><td>30-day free trial, then billed automatically unless cancelled</td><td>Email + domain, free tier runs indefinitely</td></tr>
<tr><td>Cookies</td><td>None (Plausible's own claim)</td><td>None; see "How it stays cookieless" below</td></tr>
</table>
<h2>What's free with Railyard Insights, exactly</h2>
<ul class="points">
<li>Up to ${FREE} stored events a month, per site, for as long as you use it — not a trial that starts billing automatically.</li>
<li>No credit card to sign up: an email address and a domain get you the tag and a private dashboard link at once.</li>
<li>Past the free cap, new events are still counted (so your numbers don't silently go dark) but not stored until the 1st of next month; nothing already stored is ever removed.</li>
<li>Delete the site and every stored event yourself, from the dashboard, at any time.</li>
</ul>
<h2>What's in the dashboard</h2>
<ul class="points">
<li>Visitors and page views for the last 7, 30 or 90 days, including how many viewed 2 or more pages.</li>
<li>Top pages, top referrers, and <code>utm_source</code> per campaign link.</li>
<li>Custom events with <code>railyard.track("signup")</code>.</li>
<li>Real-visitor page speed: LCP, INP and CLS.</li>
</ul>
<h2>Where Plausible is ahead</h2>
<p>Plausible's paid plans add funnels, a Stats API, Search Console integration and longer data retention (per its own pricing page) that Railyard Insights' free tier does not have. If you need those specifically, Plausible is the better fit; if you want a free tier that keeps working without a trial clock, Railyard Insights is built for that search.</p>
<h2>How it stays cookieless</h2>
<p>A visitor is a one-way hash of IP address, browser and a secret that changes every UTC day; the IP address itself is never stored. No cookies, no local storage, no cross-day fingerprinting. Browsers with Do Not Track on send nothing. See <a href="/privacy">the full privacy page</a> for every field the tag sends.</p>
<p class="small">Plausible's prices and plan limits checked on plausible.io's own pricing page, 2026-10-15; Plausible may change them at any time.</p>
${seoFooter}`;
  return shell({
    title: "Plausible alternative: free tier that's not a trial | Railyard Insights",
    description: "Plausible Starter is $9/mo for 10,000 pageviews with a 30-day trial. Railyard Insights is free up to " + FREE + " stored events a month, no card, no trial clock.",
    body,
    canonical: `${CANONICAL}/plausible-alternative`,
    tag,
  });
}

export function fathomAlternativePage({ tag } = {}) {
  const body = `<h1>A Fathom Analytics alternative with no card required</h1>
<p class="muted">Fathom Analytics is another established cookieless analytics tool. Checked on Fathom's own pricing page: its entry plan is <b>$15/month for up to 100,000 monthly pageviews</b> (covering up to 50 sites on every plan), billed after a <b>7-day free trial that requires a card up front</b>. Railyard Insights is free for up to ${FREE} stored events a month per site, and never asks for a card to start.</p>
<h2>Side by side</h2>
<table><tr><th></th><th>Fathom (entry plan)</th><th>Railyard Insights (free)</th></tr>
<tr><td>Price</td><td>$15/month, billed</td><td>$0, no card</td></tr>
<tr><td>Monthly cap on that plan</td><td>100,000 pageviews</td><td>${FREE} stored events</td></tr>
<tr><td>Trial</td><td>7 days, card required up front</td><td>No trial: the free tier itself runs indefinitely</td></tr>
<tr><td>Cookies</td><td>None (Fathom's own claim)</td><td>None; see "How it stays cookieless" below</td></tr>
</table>
<h2>What's free with Railyard Insights, exactly</h2>
<ul class="points">
<li>Up to ${FREE} stored events a month, per site, for as long as you use it.</li>
<li>No credit card to sign up: an email address and a domain get you the tag and a private dashboard link at once.</li>
<li>Past the free cap, new events are still counted but not stored until the 1st of next month; nothing already stored is ever removed.</li>
<li>Delete the site and every stored event yourself, from the dashboard, at any time.</li>
</ul>
<h2>Where Fathom is ahead</h2>
<p>Fathom's paid plans cover up to 50 sites per account and far higher pageview ceilings (its own pricing page lists tiers up to 25,000,000 monthly pageviews) — a better fit for an agency running many high-traffic sites on one bill. Railyard Insights' free tier is scoped to a single site's events and is built for the search for a no-card starting point, not for that scale.</p>
<h2>How it stays cookieless</h2>
<p>A visitor is a one-way hash of IP address, browser and a secret that changes every UTC day; the IP address itself is never stored. No cookies, no local storage, no cross-day fingerprinting. Browsers with Do Not Track on send nothing. See <a href="/privacy">the full privacy page</a> for every field the tag sends.</p>
<p class="small">Fathom's prices and plan limits checked on usefathom.com's own pricing page, 2026-10-15; Fathom may change them at any time.</p>
${seoFooter}`;
  return shell({
    title: "Fathom Analytics alternative, no card required | Railyard Insights",
    description: "Fathom's entry plan is $15/mo with a 7-day trial that needs a card. Railyard Insights is free up to " + FREE + " stored events a month, no card at all.",
    body,
    canonical: `${CANONICAL}/fathom-analytics-alternative`,
    tag,
  });
}

const list = (title, rows, cols) =>
  `<h2>${esc(title)}</h2>${rows.length ? `<table><tr>${cols.map((c) => `<th${c.r ? ' class="r"' : ""}>${esc(c.h)}</th>`).join("")}</tr>${rows.map((r) => `<tr>${cols.map((c) => `<td${c.r ? ' class="r"' : ""}>${esc(c.v(r))}</td>`).join("")}</tr>`).join("")}</table>` : `<p class="small">Nothing yet.</p>`}`;

/** Agent failures sit apart from page views and custom events. The prompt is not stored. */
function agentFailureSection(rows) {
  const table = list("Agent failures", rows ?? [], [
    { h: "When", v: (r) => r.ts },
    { h: "Agent", v: (r) => r.agent },
    { h: "Model", v: (r) => r.model || "none" },
    { h: "Error", v: (r) => r.error },
  ]);
  return `${table}<p class="small">Each row is one agent that failed: the agent, the model, and the error name. A prompt or a key is not stored. Send one with <code>railyard.track("$agent_failure", { agent: "relay", model: "name", error: "TimeoutError" })</code>. Faultline gets the same three fields as the issue.</p>`;
}

// Google's "good" thresholds for LCP, INP and CLS at the 75th percentile.
const SPEED_GOOD = { lcp: 2500, inp: 200, cls: 0.1 };
const grade = (k, v) => (v <= SPEED_GOOD[k] ? "good" : "needs work");

function speedSection(speed) {
  if (!speed || !speed.samples) return `<h2>Page speed</h2><p class="small">Nothing yet. LCP, INP and CLS arrive from real visits when a visitor leaves or hides a page.</p>`;
  const cells = [
    ["LCP", "lcp", speed.lcp, (v) => `${(v / 1000).toFixed(1)} s`],
    ["INP", "inp", speed.inp, (v) => `${Math.round(v)} ms`],
    ["CLS", "cls", speed.cls, (v) => v.toFixed(2)],
  ].filter((c) => c[2] != null);
  return `<h2>Page speed</h2><div class="grid">${cells.map(([label, k, v, f]) => `<div class="stat"><b>${esc(f(v))}</b>${label}, ${grade(k, v)}</div>`).join("")}</div>
<p class="small">75th percentile of ${n(speed.samples)} page views from real visitors. Good is LCP under 2.5 s, INP under 200 ms, CLS under 0.1.</p>`;
}

const funnelSection = (funnel) => {
  const steps = funnel?.steps ?? [];
  if (!steps.length || !steps[0].visitors) return "";
  const rows = steps.map((s, i) => `<tr><td>${esc(s.step)}</td><td class="r">${n(s.visitors)}</td><td class="r">${i === 0 ? "-" : `${s.rate ?? 0}%`}</td></tr>`).join("");
  return `<h2>Funnel</h2><p class="small">Same-day path order: home, then your next 2 most-visited pages. Each row counts visitor-days that reached that step after the one above it.</p>
<table><tr><th>Step</th><th class="r">Visitors</th><th class="r">Reached from previous</th></tr>${rows}</table>`;
};

export function dashboardPage({ token, origin, key, site, stats, speed, funnel, days, usage, fresh, capLine, billingReady = false }) {
  const cur = stats;
  const snippet = snippetFor(origin, key);
  const waiting = usage.all === 0;
  const showUpgrade = billingReady && !site.paid && Boolean(capLine);
  const freshLine =
    fresh === "mailed"
      ? "Your site is added. Bookmark this page: it is your private dashboard, and we emailed you the link."
      : fresh === "upgraded"
        ? "Thanks — your payment is confirmed. The higher monthly limit applies as soon as Dodo's confirmation arrives (usually within a few seconds)."
        : fresh
          ? "Your site is added. Bookmark this page: it is your private dashboard."
          : null;
  const body = `<h1>${esc(site.label)}</h1>
${freshLine ? `<p class="ok">${esc(freshLine)}</p>` : ""}
${waiting ? `<h2>Step 1: add the tag</h2><p>Paste this into the <code>&lt;head&gt;</code> of every page of ${esc(site.label)}, deploy, then open your site in a browser. Your first page view shows up here within a few seconds of a reload.</p><pre>${esc(snippet)}</pre>` : ""}
<nav class="days">Last: ${[7, 30, 90].map((d) => (d === days ? `<b>${d} days</b>` : `<a href="/d/${esc(token)}?days=${d}">${d} days</a>`)).join(" ")}</nav>
<div class="grid">
<div class="stat"><b>${n(cur.visitors)}</b>visitors</div><div class="stat"><b>${n(cur.pageviews)}</b>page views</div>
<div class="stat"><b>${n(cur.engaged)}</b>viewed 2+ pages</div><div class="stat"><b>${n(cur.converted)}</b>sent a custom event</div>
</div>
<p class="small">${n(usage.month)} of ${n(site.events_limit)} stored events used this month.${site.paid ? " (paid plan)" : ""}${capLine ? ` ${esc(capLine)}` : ""}</p>
${
  showUpgrade
    ? `<form method="post" action="/d/${esc(token)}/upgrade" style="margin:10px 0"><button type="submit">Upgrade for more stored events</button></form>`
    : ""
}
${list("Top pages", cur.top_pages, [{ h: "Page", v: (r) => r.url }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("Top referrers", cur.top_referrers, [{ h: "Site", v: (r) => r.host }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("UTM sources", cur.utm_sources, [{ h: "utm_source", v: (r) => r.source }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("Custom events", cur.events, [{ h: "Event", v: (r) => r.name }, { h: "Count", v: (r) => n(r.n), r: 1 }])}
${funnelSection(funnel)}
${agentFailureSection(cur.agent_failures)}
${speedSection(speed)}
<h2>Session replay</h2>
<p class="small">${site.paid ? `Watch back what visitors do on the page.` : `A paid-tier feature: record and watch back what visitors do on the page.`} <a href="/d/${esc(token)}/replay">${site.paid ? "Open session replay" : "Learn more"}</a></p>
${waiting ? "" : `<h2>Your tag</h2><pre>${esc(snippet)}</pre>`}
<p class="small">Count a sign-up or purchase with <code>railyard.track("signup")</code> after the tag loads. Visitors are counted per day, without cookies.</p>
<h2>Delete this site</h2>
<form method="post" action="/d/${esc(token)}/delete" style="display:grid;gap:10px">
<label>Type <b>${esc(site.label)}</b> to delete the site and every stored event. This cannot be undone.<input name="confirm" required autocomplete="off"></label>
<button class="danger" type="submit">Delete this site and its data</button></form>`;
  return shell({ title: `${site.label} analytics | Railyard Insights`, description: "Private dashboard", body, robots: "noindex,nofollow" });
}

// Live example (2026-10-16): a public, read-only view of this site's own
// numbers -- the same dashboard every Railyard Insights sign-up gets --
// so a visitor can see the product work on real traffic before giving an
// email address. No PII: no site labels, emails or dashboard tokens of any
// customer appear here, only this landing page's own aggregate counts.
export function liveDemoPage({ stats, speed, funnel, days, tag } = {}) {
  const cur = stats;
  const body = `<h1>A live example, not a mockup</h1>
<p class="muted">This is the real traffic to insights.ereboslab.com itself over the last ${days} days, read with the same dashboard every sign-up gets. Nothing here is a screenshot or a made-up number.</p>
<nav class="days">Last: ${[7, 30, 90].map((d) => (d === days ? `<b>${d} days</b>` : `<a href="/live-demo?days=${d}">${d} days</a>`)).join(" ")}</nav>
<div class="grid">
<div class="stat"><b>${n(cur.visitors)}</b>visitors</div><div class="stat"><b>${n(cur.pageviews)}</b>page views</div>
<div class="stat"><b>${n(cur.engaged)}</b>viewed 2+ pages</div><div class="stat"><b>${n(cur.converted)}</b>sent a custom event</div>
</div>
${list("Top pages", cur.top_pages, [{ h: "Page", v: (r) => r.url }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${list("Top referrers", cur.top_referrers, [{ h: "Site", v: (r) => r.host }, { h: "Views", v: (r) => n(r.n), r: 1 }])}
${funnelSection(funnel)}
${agentFailureSection(cur.agent_failures)}
${speedSection(speed)}
<h2>Get the same dashboard for your own site</h2>
<form class="signup" method="post" action="/api/sites">
<label>Your site's domain<input name="domain" placeholder="shop.example.com" required autocomplete="url"></label>
<label>Your email (we send the dashboard link here)<input type="email" name="email" placeholder="you@company.com" required autocomplete="email"></label>
<button type="submit">Get my tag and dashboard</button>
<p class="small">No password and no card. Free for up to ${FREE} stored events a month per site.</p>
</form>`;
  return shell({
    title: "Live example dashboard, real traffic | Railyard Insights",
    description: "Real, live visitor and page-speed numbers from this site's own analytics -- the exact dashboard every Railyard Insights sign-up gets, before you give an email address.",
    body,
    canonical: `${CANONICAL}/live-demo`,
    tag,
  });
}

export function messagePage(title, text, status = "") {
  return shell({ title: `${title} | Railyard Insights`, description: title, robots: "noindex", body: `<h1>${esc(title)}</h1>${status ? `<p class="${status}">${esc(text)}</p>` : `<p>${esc(text)}</p>`}<p><a href="/">Back to Railyard Insights</a></p>` });
}

export const ROBOTS = `User-agent: *\nAllow: /\nDisallow: /d/\nDisallow: /api/\nSitemap: ${CANONICAL}/sitemap.xml\n`;
const SEO_PATHS = ["/cookieless-analytics", "/google-analytics-alternative", "/plausible-alternative", "/fathom-analytics-alternative"];
export const SITEMAP = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>${CANONICAL}/</loc><lastmod>2026-10-12</lastmod></url><url><loc>${CANONICAL}/privacy</loc><lastmod>2026-10-12</lastmod></url><url><loc>${CANONICAL}/live-demo</loc><lastmod>2026-10-16</lastmod></url>${SEO_PATHS.map((p) => `<url><loc>${CANONICAL}${p}</loc><lastmod>2026-10-14</lastmod></url>`).join("")}</urlset>\n`;
