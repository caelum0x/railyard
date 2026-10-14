// Session replay dashboard pages (2026-10-15): a settings panel + session
// list at /d/<token>/replay, and a player at /d/<token>/replay/<sessionId>
// that loads the vendored rrweb-player bundle. Same private-token auth and
// the same look as the rest of the Insights dashboard (src/pages.mjs' shell).

import { esc, shell } from "../pages.mjs";
import { DEFAULT_MASK_TEXT_SELECTOR, resolveReplayConfig } from "./config.mjs";
import { RRWEB_PLAYER_CSS } from "../vendor/rrweb-player.generated.css.mjs";

const n = (v) => Number(v ?? 0).toLocaleString("en-US");
const fmtMs = (ms) => (ms >= 60_000 ? `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s` : `${Math.round((ms ?? 0) / 1000)}s`);
const fmtBytes = (b) => (b >= 1_000_000 ? `${(b / 1_000_000).toFixed(1)} MB` : `${Math.round((b ?? 0) / 1000)} KB`);

function settingsForm(token, cfg, { paid, billingReady, upgradeHref }) {
  if (!paid) {
    return `<div class="stat" style="padding:16px">
<p><b>Session replay</b> is a paid feature: it records what visitors do on the page (DOM changes, clicks, scroll, input — masked by default) so you can watch a real session back.</p>
${billingReady ? `<form method="post" action="${esc(upgradeHref)}"><button type="submit">Upgrade to turn on session replay</button></form>` : `<p class="small">Not available to turn on yet.</p>`}
</div>`;
  }
  return `<form method="post" action="/d/${esc(token)}/replay/settings" style="display:grid;gap:10px;background:#fff;border:1px solid #e3e3e3;border-radius:12px;padding:16px">
<label style="display:flex;align-items:center;gap:8px"><input type="checkbox" name="enabled" ${cfg.enabled ? "checked" : ""}> Record session replay for this site</label>
<label>Sample rate (share of visits recorded, 0.01 to 1)<input name="sample_rate" type="number" min="0.01" max="1" step="0.01" value="${esc(cfg.sample_rate)}"></label>
<label style="display:flex;align-items:center;gap:8px"><input type="hidden" name="mask_all_inputs" value="off"><input type="checkbox" name="mask_all_inputs" value="on" ${cfg.mask_all_inputs ? "checked" : ""}> Mask all form inputs (recommended, on by default)</label>
<label>Mask text matching this selector (<code>*</code> masks every text node)<input name="mask_text_selector" value="${esc(cfg.mask_text_selector)}" placeholder="${esc(DEFAULT_MASK_TEXT_SELECTOR)}"></label>
<p class="small">Replay is off by default, samples a share of visits, drops nothing already recorded when you turn it off, and never runs for a visitor with Do Not Track or Global Privacy Control on.</p>
<button type="submit">Save settings</button>
</form>`;
}

function sessionRow(token, s) {
  const when = new Date(s.started_at).toLocaleString("en-US", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  return `<tr>
<td><a href="/d/${esc(token)}/replay/${esc(s.session_id)}">${esc(when)}</a></td>
<td>${esc(s.url ?? "")}</td>
<td>${esc(s.country ?? "")}</td>
<td class="r">${esc(fmtMs(s.duration_ms))}</td>
<td class="r">${n(s.event_count)}</td>
<td class="r">${esc(fmtBytes(s.bytes))}</td>
<td>${s.truncated ? "truncated" : s.ended ? "ended" : "recording"}</td>
</tr>`;
}

export function replaySessionsPage({ token, origin, site, days, sessions, paid, billingReady, saved }) {
  const cfg = resolveReplayConfig(site);
  const body = `<h1>Session replay — ${esc(site.label)}</h1>
<p><a href="/d/${esc(token)}">&larr; Back to analytics</a></p>
${saved ? `<p class="ok">Settings saved.</p>` : ""}
<h2>Settings</h2>
${settingsForm(token, cfg, { paid, billingReady, upgradeHref: `/d/${esc(token)}/upgrade` })}
<h2>Recorded sessions</h2>
<nav class="days">Last: ${[7, 30, 90].map((d) => (d === days ? `<b>${d} days</b>` : `<a href="/d/${esc(token)}/replay?days=${d}">${d} days</a>`)).join(" ")}</nav>
${
  sessions.length
    ? `<table><tr><th>Started</th><th>Page</th><th>Country</th><th class="r">Length</th><th class="r">Events</th><th class="r">Size</th><th>Status</th></tr>${sessions.map((s) => sessionRow(token, s)).join("")}</table>`
    : `<p class="small">${paid && cfg.enabled ? "No sessions recorded yet — they show up here within a few seconds of a sampled visit." : "Turn replay on above to start recording sessions."}</p>`
}`;
  return shell({ title: `Session replay — ${site.label} | Railyard Insights`, description: "Private session replay dashboard", body, robots: "noindex,nofollow" });
}

export function replayPlayerPage({ token, origin, site, session }) {
  const when = new Date(session.started_at).toLocaleString("en-US", { dateStyle: "medium", timeStyle: "short" });
  const body = `<h1>Session replay — ${esc(site.label)}</h1>
<p><a href="/d/${esc(token)}/replay">&larr; Back to sessions</a></p>
<p class="small">${esc(when)} &middot; ${esc(session.url ?? "")} &middot; ${esc(fmtMs(session.duration_ms))} &middot; ${n(session.event_count)} events${session.truncated ? " &middot; truncated at the size cap" : ""}</p>
<div id="ry-player" data-events-url="/api/d/${esc(token)}/replay/${esc(session.session_id)}/events" style="margin:16px 0"></div>
<p class="small" id="ry-player-status">Loading player…</p>
<style>${RRWEB_PLAYER_CSS}</style>
<script src="/replay-player.js"></script>
<script src="/replay-viewer.js"></script>`;
  return shell({ title: `Session replay — ${site.label} | Railyard Insights`, description: "Private session replay player", body, robots: "noindex,nofollow" });
}
