// Custom domains for shipped apps (Wave Q QY1.5, I6). `railyard domains add`
// asks the console, which attaches the hostname to the app's production
// Worker on the org's own Cloudflare account with the org's Cloudflare grant
// (the ship-on-push OAuth step, QY1.1): the access token is opened, used and
// dropped inside this function; it never reaches the CLI, a page or a log.
// Cloudflare issues the TLS certificate; the record keeps its state for the
// Scribase Deploy tab. An org without a grant gets 409 no_cloudflare_grant,
// and the CLI then does the same with the local Cloudflare login and reports
// the outcome here (record), so the tab shows it either way.

import { cloudflareAccess, ShipError, type ShipDeps } from "./service.ts";
import { addDomain, cleanHostname, cloudflareDomains, DomainError, listDomains, removeDomain, type CloudflareDomains } from "./cf-domains.mjs";
import type { AppDomain } from "./types.ts";

export const MAX_DOMAINS_PER_ORG = 50;
const APP = /^[a-z0-9][a-z0-9-]{0,39}$/;

/** The Worker a production deploy of `app` goes to (the CLI's workerName for the production branch). */
export function productionWorker(app: string, productionBranch = "main"): string {
  const clean = (s: string) => s.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "");
  const base = clean(app).slice(0, 40) || "app";
  if (!productionBranch || ["main", "master", "HEAD"].includes(productionBranch)) return base;
  return `${base}-${clean(productionBranch)}`.slice(0, 63).replace(/-+$/, "");
}

async function workerFor(deps: ShipDeps, orgId: string, app: string): Promise<string> {
  const link = (await deps.store.listLinks(orgId)).find((l) => l.app === app);
  return productionWorker(app, link?.production_branch ?? "main");
}

async function grantClient(deps: ShipDeps, orgId: string, now: Date): Promise<CloudflareDomains | null> {
  const conn = await deps.store.getCloudflare(orgId);
  if (!conn) return null;
  const { token } = await cloudflareAccess(deps, conn, now);
  return cloudflareDomains({ token, accountId: conn.account_id, apiBase: deps.cloudflare.cfg.apiBase, fetchImpl: deps.cloudflare.fetchImpl });
}

function input(body: unknown): { app: string; hostname: string } {
  const b = (body ?? {}) as Record<string, unknown>;
  const app = typeof b.app === "string" ? b.app : "";
  if (!APP.test(app)) throw new ShipError("Name the app (lowercase letters, digits, dashes).", 400);
  const hostname = cleanHostname(b.hostname);
  if (!hostname) throw new ShipError("Give a full hostname such as app.example.com (not a workers.dev name).", 400);
  return { app, hostname };
}

const noGrant = () => Object.assign(new ShipError("Cloudflare is not connected for this organization in the console; the CLI uses your local Cloudflare login instead.", 409), { code: "no_cloudflare_grant" });

function wrap(e: unknown): never {
  if (e instanceof DomainError) throw new ShipError(e.message, e.status);
  throw e;
}

async function ownApp(deps: ShipDeps, orgId: string, app: string): Promise<void> {
  if (!(await deps.repo.findAppByName(orgId, app))) throw new ShipError("There is no app with that name in your organization.", 404);
}

/** Attaches a hostname to the app's production Worker with the org's grant. */
export async function addAppDomain(deps: ShipDeps, orgId: string, body: unknown, now: Date): Promise<AppDomain> {
  const { app, hostname } = input(body);
  await ownApp(deps, orgId, app);
  const mine = await deps.store.listDomains(orgId);
  const taken = mine.find((d) => d.hostname === hostname);
  if (taken && taken.app !== app) throw new ShipError(`${hostname} is attached to ${taken.app}. Remove it there first.`, 409);
  if (!taken && mine.length >= MAX_DOMAINS_PER_ORG) throw new ShipError(`An organization can attach ${MAX_DOMAINS_PER_ORG} domains.`, 409);
  const cf = await grantClient(deps, orgId, now);
  if (!cf) throw noGrant();
  const worker = await workerFor(deps, orgId, app);
  const d = await addDomain(cf, hostname, worker).catch(wrap);
  const row: AppDomain = { org_id: orgId, app, hostname, worker, cf_domain_id: d.id, zone_name: d.zone_name, state: d.state, checked_at: now.toISOString(), created_at: taken?.created_at ?? now.toISOString() };
  await deps.store.saveDomain(row);
  return row;
}

/** Detaches a hostname (idempotent: removing what is not attached answers removed: false). */
export async function removeAppDomain(deps: ShipDeps, orgId: string, body: unknown, now: Date): Promise<{ hostname: string; removed: boolean }> {
  const { app, hostname } = input(body);
  const mine = (await deps.store.listDomains(orgId)).find((d) => d.hostname === hostname);
  if (mine && mine.app !== app) throw new ShipError(`${hostname} belongs to ${mine.app}, not ${app}.`, 409);
  const cf = await grantClient(deps, orgId, now);
  if (!cf) throw noGrant();
  // Only a hostname this org attached is detached on Cloudflare; anything else is left alone.
  const r = mine ? await removeDomain(cf, hostname).catch(wrap) : { hostname, removed: false };
  const dropped = await deps.store.deleteDomain(orgId, hostname);
  return { hostname, removed: r.removed || dropped };
}

/** The CLI's report after it attached or detached with the local login (no grant here). */
export async function recordAppDomain(deps: ShipDeps, orgId: string, body: unknown, now: Date): Promise<{ recorded: boolean }> {
  const { app, hostname } = input(body);
  await ownApp(deps, orgId, app);
  const b = body as Record<string, unknown>;
  if (b.removed === true) return { recorded: await deps.store.deleteDomain(orgId, hostname) };
  const state = b.state === "active" || b.state === "error" ? b.state : "pending";
  const text = (v: unknown, max: number) => (typeof v === "string" && v.length <= max ? v : null);
  const existing = (await deps.store.listDomains(orgId)).find((d) => d.hostname === hostname);
  if (existing && existing.app !== app) throw new ShipError(`${hostname} is attached to ${existing.app}. Remove it there first.`, 409);
  await deps.store.saveDomain({
    org_id: orgId,
    app,
    hostname,
    worker: await workerFor(deps, orgId, app),
    cf_domain_id: text(b.cf_domain_id, 64),
    zone_name: text(b.zone_name, 253),
    state,
    checked_at: now.toISOString(),
    created_at: existing?.created_at ?? now.toISOString(),
  });
  return { recorded: true };
}

/**
 * The org's domains (one app, or all), with certificate states refreshed from
 * Cloudflare when the org has a grant; otherwise as last recorded.
 */
export async function appDomains(deps: ShipDeps, orgId: string, app: string | null, now: Date): Promise<AppDomain[]> {
  const rows = await deps.store.listDomains(orgId, app);
  if (rows.length === 0 || rows.every((r) => r.state === "active")) return rows;
  let cf: CloudflareDomains | null = null;
  try {
    cf = await grantClient(deps, orgId, now);
  } catch {
    return rows;
  }
  if (!cf) return rows;
  const out: AppDomain[] = [];
  for (const worker of [...new Set(rows.map((r) => r.worker))]) {
    const live = await listDomains(cf, worker).catch(() => null);
    for (const r of rows.filter((x) => x.worker === worker)) {
      const found = live?.find((d) => d.hostname === r.hostname);
      const next: AppDomain = live ? { ...r, state: found ? found.state : "error", checked_at: now.toISOString() } : r;
      if (next.state !== r.state) await deps.store.saveDomain(next);
      out.push(next);
    }
  }
  return out.sort((a, b) => a.hostname.localeCompare(b.hostname));
}
