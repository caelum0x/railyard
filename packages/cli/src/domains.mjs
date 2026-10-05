// Custom domains for shipped apps (Wave Q QY1.5, I6), on the customer's own
// Cloudflare account through Cloudflare's public Workers Custom Domains API:
//   GET    /zones?name=<zone>                              the zone that holds the hostname
//   PUT    /accounts/{account}/workers/domains             attach {environment, hostname, service, zone_id}
//   GET    /accounts/{account}/workers/domains?service=    what is attached
//   DELETE /accounts/{account}/workers/domains/{id}        detach
//   GET    /zones/{zone}/ssl/certificate_packs?status=all  the certificate Cloudflare issues for it
// Cloudflare issues and renews the TLS certificate itself; a domain is
// "active" once its certificate is. The token (an OAuth access token from the
// console's Cloudflare grant, or the local wrangler login) is sent in the
// Authorization header only; it is never printed, logged or put in an error.
//
// One file, no dependencies: the CLI uses it with the local login, the
// console with the organization's grant (byte copy in
// apps/console/lib/ship/cf-domains.mjs, kept equal by a drift test).

export const CF_API = "https://api.cloudflare.com/client/v4";
const HOSTNAME = /^(?=.{4,253}$)(?!-)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/;

export class DomainError extends Error {
  /** @param {string} message @param {number} status */
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

/** A hostname as Cloudflare takes it (lower case, no scheme, no wildcard), or null. */
export function cleanHostname(raw) {
  const h = String(raw ?? "").trim().toLowerCase().replace(/\.$/, "");
  return HOSTNAME.test(h) && !h.endsWith(".workers.dev") ? h : null;
}

/** The zones a hostname could live in, longest first (app.shop.example.com -> shop.example.com, example.com). */
export function zoneCandidates(hostname) {
  const parts = hostname.split(".");
  const out = [];
  for (let i = 0; i <= parts.length - 2; i += 1) out.push(parts.slice(i).join("."));
  return out;
}

/** A certificate pack status in words the tab and CLI show. */
export function domainState(certStatus) {
  if (certStatus === "active") return "active";
  if (certStatus === null || certStatus === undefined) return "pending";
  if (/^(pending|initializing|validation|pending_validation|pending_issuance|pending_deployment)/.test(certStatus)) return "pending";
  return "error";
}

/**
 * @param {{ token: string, accountId: string, fetchImpl?: typeof fetch, apiBase?: string }} opts
 */
export function cloudflareDomains(opts) {
  const base = (opts.apiBase ?? CF_API).replace(/\/$/, "");
  const doFetch = opts.fetchImpl ?? fetch;
  const scrub = (s) => String(s).split(opts.token).join("[redacted]").slice(0, 300);
  async function call(method, path, body) {
    let res;
    try {
      res = await doFetch(`${base}${path}`, {
        method,
        headers: { authorization: `Bearer ${opts.token}`, "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (e) {
      throw new DomainError(scrub(`Cloudflare could not be reached: ${e.message}`), 502);
    }
    const json = await res.json().catch(() => ({}));
    if (res.status === 404 && method === "DELETE") return null;
    if (!res.ok || json.success === false) {
      const msg = Array.isArray(json.errors) && json.errors[0]?.message ? json.errors[0].message : `Cloudflare answered ${res.status}`;
      const status = res.status === 401 || res.status === 403 ? 403 : res.status >= 500 ? 502 : 400;
      throw new DomainError(scrub(status === 403 ? `Cloudflare refused: ${msg}. Grant domain access (zone read and certificates) and try again.` : `Cloudflare refused: ${msg}`), status);
    }
    return json.result;
  }
  const api = {
    /** The account's zone holding `hostname`. */
    async zoneFor(hostname) {
      for (const name of zoneCandidates(hostname)) {
        const found = await call("GET", `/zones?name=${encodeURIComponent(name)}&account.id=${encodeURIComponent(opts.accountId)}`);
        const zone = Array.isArray(found) ? found.find((z) => z.name === name) : null;
        if (zone) return { id: zone.id, name: zone.name };
      }
      throw new DomainError(`${hostname} is not in a zone of this Cloudflare account. Add the domain to the account first.`, 404);
    },
    /** @returns {Promise<{id: string, hostname: string, service: string, zone_id: string, zone_name: string}[]>} */
    async list(service) {
      const q = service ? `?service=${encodeURIComponent(service)}` : "";
      const r = await call("GET", `/accounts/${encodeURIComponent(opts.accountId)}/workers/domains${q}`);
      return (Array.isArray(r) ? r : []).map((d) => ({ id: d.id, hostname: d.hostname, service: d.service, zone_id: d.zone_id, zone_name: d.zone_name }));
    },
    async attach(hostname, service, zoneId) {
      const d = await call("PUT", `/accounts/${encodeURIComponent(opts.accountId)}/workers/domains`, { environment: "production", hostname, service, zone_id: zoneId });
      return { id: d.id, hostname: d.hostname, service: d.service, zone_id: d.zone_id, zone_name: d.zone_name };
    },
    async detach(id) {
      await call("DELETE", `/accounts/${encodeURIComponent(opts.accountId)}/workers/domains/${encodeURIComponent(id)}`);
    },
    /** The status of the certificate covering `hostname` in `zoneId`, or null when none is issued yet. */
    async certStatus(zoneId, hostname) {
      const packs = await call("GET", `/zones/${encodeURIComponent(zoneId)}/ssl/certificate_packs?status=all`);
      const pack = (Array.isArray(packs) ? packs : []).find((p) => Array.isArray(p.hosts) && p.hosts.includes(hostname));
      return pack ? String(pack.status) : null;
    },
  };
  return api;
}

/**
 * Attaches `hostname` to the Worker `service` (idempotent: an existing
 * attachment to the same Worker is kept; one to another Worker is refused).
 */
export async function addDomain(cf, hostname, service) {
  const host = cleanHostname(hostname);
  if (!host) throw new DomainError("Give a full hostname such as app.example.com (not a workers.dev name).", 400);
  const existing = (await cf.list()).find((d) => d.hostname === host);
  if (existing && existing.service !== service) throw new DomainError(`${host} already points at another Worker (${existing.service}). Remove it there first.`, 409);
  const zone = existing ? { id: existing.zone_id, name: existing.zone_name } : await cf.zoneFor(host);
  const d = existing ?? (await cf.attach(host, service, zone.id));
  const cert = await cf.certStatus(d.zone_id, host).catch(() => null);
  return { id: d.id, hostname: host, service, zone_id: d.zone_id, zone_name: d.zone_name, state: domainState(cert), created: !existing };
}

/** Detaches `hostname` (idempotent: a domain that is not attached is a no-op). */
export async function removeDomain(cf, hostname) {
  const host = cleanHostname(hostname);
  if (!host) throw new DomainError("Give the full hostname to remove.", 400);
  const existing = (await cf.list()).find((d) => d.hostname === host);
  if (!existing) return { hostname: host, removed: false };
  await cf.detach(existing.id);
  return { hostname: host, removed: true };
}

/** The domains of one Worker with their certificate state. */
export async function listDomains(cf, service) {
  const list = await cf.list(service);
  return Promise.all(list.map(async (d) => ({ ...d, state: domainState(await cf.certStatus(d.zone_id, d.hostname).catch(() => null)) })));
}
