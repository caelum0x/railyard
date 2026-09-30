// HTTP handlers for ship on push (PY3.1), framework-free: the route files
// under app/api/v1/github/** only pass the request through.

import type { Repo } from "../repo.ts";
import { orgForBearer } from "../tokens.ts";
import { CloudflareError } from "./cloudflare.ts";
import { GithubError } from "./github.ts";
import type { Jwk } from "./jwt.ts";
import { ciReport, ciSession, cloudflareCallback, githubSetup, githubUserCallback, go, linkStatus, ShipError, startLink, webhook, type ShipDeps } from "./service.ts";

export const NOT_CONFIGURED = "Ship on push is not switched on for this console yet.";

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] as string);

/** A plain page for the browser steps; the message never holds a token. */
export function page(title: string, message: string, status = 200): Response {
  // Escape first, then turn GitHub pull request links into anchors (their characters survive escaping).
  const text = escapeHtml(message).replace(/https:\/\/github\.com\/[\w./-]+[\w/]/g, (u) => `<a href="${u}">${u}</a>`);
  const body = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escapeHtml(title)}</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:15vh auto;padding:0 1.5rem;color:#1d2330;background:#f6f7f9}h1{font-size:1.25rem}a{color:#2b5bd7}</style></head>
<body><h1>${escapeHtml(title)}</h1><p>${text}</p><p>You can close this tab; the terminal shows the same.</p></body></html>`;
  return new Response(body, { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store", "referrer-policy": "no-referrer" } });
}

function failure(e: unknown, html: boolean): Response {
  const known = e instanceof ShipError || e instanceof GithubError || e instanceof CloudflareError;
  if (!known) console.error(`ship on push: ${(e as Error)?.message ?? e}`);
  const status = known ? (e as { status: number }).status : 500;
  const message = known ? (e as Error).message : "Railyard could not finish. Try again in a minute.";
  return html ? page("Not linked", message, status) : json({ error: message }, status);
}

const redirectOrPage = (r: { redirect: string } | { done: string }) => ("redirect" in r ? Response.redirect(r.redirect, 302) : page("Linked", r.done));

export async function linkStartH(deps: ShipDeps | null, repo: Repo, req: Request, now: Date): Promise<Response> {
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  if (!deps) return json({ error: NOT_CONFIGURED }, 503);
  try {
    return json(await startLink(deps, who.orgId, await req.json().catch(() => null), now), 201);
  } catch (e) {
    return failure(e, false);
  }
}

export async function linkStatusH(deps: ShipDeps | null, repo: Repo, req: Request): Promise<Response> {
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  if (!deps) return json({ error: NOT_CONFIGURED }, 503);
  try {
    return json(await linkStatus(deps, who.orgId, new URL(req.url).searchParams.get("id")));
  } catch (e) {
    return failure(e, false);
  }
}

export async function goH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return page("Not available", NOT_CONFIGURED, 503);
  try {
    return redirectOrPage(await go(deps, new URL(req.url).searchParams.get("s"), now));
  } catch (e) {
    return failure(e, true);
  }
}

export async function setupH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return page("Not available", NOT_CONFIGURED, 503);
  try {
    return redirectOrPage(await githubSetup(deps, new URL(req.url).searchParams, now));
  } catch (e) {
    return failure(e, true);
  }
}

export async function githubUserCallbackH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return page("Not available", NOT_CONFIGURED, 503);
  try {
    return redirectOrPage(await githubUserCallback(deps, new URL(req.url).searchParams, now));
  } catch (e) {
    return failure(e, true);
  }
}

export async function cloudflareCallbackH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return page("Not available", NOT_CONFIGURED, 503);
  try {
    return redirectOrPage(await cloudflareCallback(deps, new URL(req.url).searchParams, now));
  } catch (e) {
    return failure(e, true);
  }
}

export async function ciSessionH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return json({ error: NOT_CONFIGURED }, 503);
  try {
    return json(await ciSession(deps, req.headers.get("authorization"), now));
  } catch (e) {
    return failure(e, false);
  }
}

export async function ciReportH(deps: ShipDeps | null, req: Request, now: Date): Promise<Response> {
  if (!deps) return json({ error: NOT_CONFIGURED }, 503);
  try {
    return json(await ciReport(deps, req.headers.get("authorization"), await req.json().catch(() => null), now));
  } catch (e) {
    return failure(e, false);
  }
}

export async function webhookH(deps: ShipDeps | null, secret: string | null, req: Request): Promise<Response> {
  if (!deps || !secret) return json({ error: NOT_CONFIGURED }, 503);
  const raw = await req.text();
  try {
    return json(await webhook(deps, secret, req.headers.get("x-github-event"), req.headers.get("x-hub-signature-256"), raw));
  } catch (e) {
    if (e instanceof SyntaxError) return json({ error: "body is not JSON" }, 400);
    return failure(e, false);
  }
}

/** GitHub's Actions OIDC keys, cached for an hour (they rotate rarely; a miss refetches once). */
export function githubOidcKeys(fetchImpl: typeof fetch = fetch, ttlMs = 3_600_000): () => Promise<Jwk[]> {
  let cache: { at: number; keys: Jwk[] } | null = null;
  return async () => {
    if (cache && Date.now() - cache.at < ttlMs) return cache.keys;
    const res = await fetchImpl("https://token.actions.githubusercontent.com/.well-known/jwks");
    const body = (await res.json().catch(() => ({}))) as { keys?: Jwk[] };
    if (!res.ok || !Array.isArray(body.keys)) throw new ShipError("GitHub's signing keys could not be read; try again.", 503);
    cache = { at: Date.now(), keys: body.keys };
    return body.keys;
  };
}
