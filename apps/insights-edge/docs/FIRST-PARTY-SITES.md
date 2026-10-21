# First-party Insights sites

Every `data-site` key Railyard Insights counts for our own properties, where the
tag actually comes from, and which repo/file to touch next time this needs an
audit. Verified live via `curl -s https://<host> | grep data-site` on 2026-10-14
unless noted.

| Site | `data-site` key | Tag source | Owning file |
|---|---|---|---|
| yokharita.com | `ry_site_yokharita_782a2c9d25ed` | shipped in the app's own layout, **and** listed as a fallback in the edge injector (belt-and-suspenders: injector skips when the tag is already present) | `yok-content-engine/apps/web/src/app/layout.tsx`; fallback entry in `railyard/apps/insights-edge/inject/wrangler.jsonc` |
| bidstage.app | `ry_site_bidstage_9afabf22e4ef` | shipped in the app's own layout only (not fronted by the edge injector — `bidstage.app` is not one of its zone routes) | `bidstage-tenders/app/layout.tsx` |
| scribase.com (+ `www.`) | `ry_site_scribase_8831be787dd5` | edge injector only — the marketing site does not carry the tag itself | `railyard/apps/insights-edge/inject/wrangler.jsonc` (`SITE_KEYS` var) |
| mail.scribase.com | `ry_site_scribase_mail_7e3c28c34586` | edge injector only | `railyard/apps/insights-edge/inject/wrangler.jsonc` (`SITE_KEYS` var) |
| menivor.com (+ `www.`) | `ry_site_menivor_89f8a6785e1a` | edge injector only — the marketing site does not carry the tag itself | `railyard/apps/insights-edge/inject/wrangler.jsonc` (`SITE_KEYS` var) |
| oxhanatelier.com (+ `www.`) | `ry_site_oxhan_4684b714b573` | shipped in the app's own layout (added 2026-10-14), **and** listed as a fallback in the edge injector, same belt-and-suspenders pattern as yokharita.com | `oxhan-atelier-fixall/apps/storefront/src/app/layout.tsx`; fallback entry in `railyard/apps/insights-edge/inject/wrangler.jsonc` |
| share.vcpeer.com | `ry_site_vcpeer_send_80cdc14ff6bc` | shipped in the app's own `_document.tsx` (added 2026-10-21, verified live via `curl`), **and** added as a fallback entry in the edge injector's `routes`/`SITE_KEYS` source — **not yet deployed** (this Worker fronts 4 other live domains, so redeploying it is a dedicated follow-up, not bundled into the vcpeer-share pass that added the tag). The site key also still needs the D1 `signups` row + `SITES` KV `site:<key>` entry (see "Adding a new first-party site" step 3 below) before it actually counts anything. | `vcpeer-share/frontend/src/pages/_document.tsx`; fallback entry (undeployed) in `railyard/apps/insights-edge/inject/wrangler.jsonc` |

## How the edge injector works

`railyard/apps/insights-edge/inject/index.mjs` is a separate Cloudflare Worker
(`railyard-insights-inject`) that sits in front of sites we either cannot cheaply
redeploy (box-hosted apps behind the tunnel) or don't want to touch per-page. On
every HTML response it checks `SITE_KEYS[hostname]` (a hostname -> `ry_site_*`
map set as a `vars` JSON string in `inject/wrangler.jsonc`) and appends the
`<script defer src="https://insights.ereboslab.com/s.js" data-site="...">` tag
right before `</body>` — but only if the page doesn't already contain a script
with that exact `src` (`HTMLRewriter` checks first). That means a site can ship
the tag itself (yokharita.com, oxhanatelier.com) and also sit in this injector's
`SITE_KEYS` as a safety net with zero risk of double-counting: if the site's own
build ever drops the tag (bad deploy, reverted commit), the injector silently
keeps counting instead of the number going quietly to zero.

**Adding a new first-party site:**
1. If the app's own repo is under our control and redeployed often, add the
   `<script>` tag directly to its root layout (see oxhanatelier.com's commit for
   the pattern) and write a render test asserting the tag is present.
2. Either way, add the hostname (+ `www.` if used) to `inject/wrangler.jsonc`'s
   `routes` array and `SITE_KEYS` var, using a `ry_site_<name>_<12-hex>` key —
   this is a cheap, idempotent fallback and the only option for sites we don't
   control the repo/build for.
3. The actual site key only starts counting once it is registered server-side
   (a D1 `signups` row + a `SITES` KV `site:<key>` entry, mirroring `createSite`
   in `apps/insights-edge/src/selfserve.mjs`) and the relevant Worker(s) are
   deployed. Both are production writes — out of scope for a code-only session;
   see the product's deploy plan for the exact commands.

## Corrections to prior research (2026-10-14)

The 2026-10-14 build plan for oxhanatelier.com described the fix as "add the
tag to the storefront's `layout.tsx`, matching the pattern in yokharita and
bidstage." That's accurate but incomplete: it missed that 3 of the 4 other
verified-live sites (scribase.com, mail.scribase.com, menivor.com) get the tag
exclusively from the edge injector, not from their own app code — there was no
`layout.tsx`-style snippet to find in those repos because there isn't one. This
doc and the injector's `SITE_KEYS` entry for oxhan were added in the same
session specifically to close that gap for the next audit.
