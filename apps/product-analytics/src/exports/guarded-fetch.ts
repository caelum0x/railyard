// The one outbound transport for customer-supplied URLs (export webhooks,
// S3-compatible endpoints, Slack/Discord/HubSpot catalogue destinations).
//
// A fetch-compatible function that:
//   1. resolves the host once and refuses blocked addresses (net-guard.ts);
//   2. connects to that exact address: the socket's lookup is replaced by one
//      that only ever answers the validated address, so no second DNS query
//      happens between the check and the connect (no DNS-rebinding window);
//   3. keeps the URL's host name for TLS (SNI and certificate checks) and for
//      the Host header, so virtual hosting and certificates work unchanged;
//   4. never follows redirects (a 3xx comes back to the caller as-is, and every
//      caller treats anything outside 2xx as a failed delivery);
//   5. uses no shared keep-alive pool (agent: false), so no socket opened for
//      one validation is ever reused for another request.
//
// This is the same pinning an undici Agent with a custom connect.lookup gives,
// done with node:http/node:https so the service takes no new dependency.

import http from "node:http";
import https from "node:https";
import type { LookupFunction } from "node:net";
import { Readable } from "node:stream";
import { DestinationRefused, resolvePinned, type GuardOptions, type Pinned } from "./net-guard.ts";

const NULL_BODY = new Set([101, 103, 204, 205, 304]);

/** A socket lookup that answers only the pinned address, whatever host it is asked for. */
export function pinnedLookup(pin: Pinned): LookupFunction {
  return ((_host: string, options: { all?: boolean } | number | undefined, cb: (...args: unknown[]) => void) => {
    const callback = typeof options === "function" ? (options as (...args: unknown[]) => void) : cb;
    const all = typeof options === "object" && options !== null && options.all === true;
    if (all) callback(null, [{ address: pin.address, family: pin.family }]);
    else callback(null, pin.address, pin.family);
  }) as unknown as LookupFunction;
}

function toResponse(res: http.IncomingMessage, method: string): Response {
  const status = res.statusCode ?? 0;
  if (status < 200 || status > 599) {
    res.destroy();
    throw new DestinationRefused(`The destination answered with an unusable status (${status}).`);
  }
  const headers = new Headers();
  for (let i = 0; i + 1 < res.rawHeaders.length; i += 2) {
    try {
      headers.append(res.rawHeaders[i], res.rawHeaders[i + 1]);
    } catch {
      // a header value fetch would refuse: drop it
    }
  }
  const noBody = NULL_BODY.has(status) || method === "HEAD";
  if (noBody) res.resume();
  const body = noBody ? null : (Readable.toWeb(res) as ReadableStream<Uint8Array>);
  return new Response(body, { status, statusText: res.statusMessage, headers });
}

async function send(input: RequestInfo | URL, init: RequestInit | undefined, opts: GuardOptions): Promise<Response> {
  const req = new Request(input, { ...init, redirect: "manual" });
  const u = new URL(req.url);
  if (u.protocol !== "https:" && u.protocol !== "http:") throw new DestinationRefused("Only http and https addresses can be reached.");
  if (u.username || u.password) throw new DestinationRefused("The address must not contain a user name or password.");
  const pin = await resolvePinned(u, opts);
  const body = req.body ? Buffer.from(await req.arrayBuffer()) : undefined;
  const headers: Record<string, string> = {};
  req.headers.forEach((v, k) => {
    headers[k] = v;
  });
  headers.host = u.host;
  if (body) headers["content-length"] = String(body.length);
  const signal = init?.signal ?? (input instanceof Request ? input.signal : undefined);
  const mod = u.protocol === "https:" ? https : http;
  return new Promise<Response>((resolve, reject) => {
    const out = mod.request(
      {
        protocol: u.protocol,
        hostname: pin.host,
        port: u.port || (u.protocol === "https:" ? 443 : 80),
        path: `${u.pathname}${u.search}`,
        method: req.method,
        headers,
        agent: false,
        lookup: pinnedLookup(pin),
        signal: signal ?? undefined,
        ...(u.protocol === "https:" ? { servername: pin.host.includes(":") || /^\d+\.\d+\.\d+\.\d+$/.test(pin.host) ? undefined : pin.host } : {}),
      },
      (res) => {
        try {
          resolve(toResponse(res, req.method));
        } catch (err) {
          reject(err);
        }
      },
    );
    out.on("error", reject);
    out.end(body);
  });
}

/** Builds a guarded fetch; options exist for tests (resolver, address rule). */
export function makeGuardedFetch(opts: GuardOptions = {}): typeof fetch {
  return ((input: RequestInfo | URL, init?: RequestInit) => send(input, init, opts)) as typeof fetch;
}

/** The default guarded fetch: system resolver, public addresses only (unless PA_EXPORTS_ALLOW_PRIVATE=1). */
export const guardedFetch: typeof fetch = makeGuardedFetch();
