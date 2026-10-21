// Railyard rail status: one aggregated health check across the money rail's
// own services (Mint, Rago, Relay, Abacus, ...) so a consumer product (or an
// operator) can ask ONE endpoint instead of probing four hosts directly
// before routing real traffic at the rail. Every service is independently
// optional -- with its URL unset it reads "not_configured" and is excluded
// from the overall verdict, so this works in any deployment that only has
// part of the rail live (invariant #5 in docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md:
// all external I/O behind an injectable transport, fake in tests).

export interface RailService {
  name: string;
  url: string | null;
}

export type ServiceState = "ok" | "down" | "not_configured";

export interface ServiceStatus {
  service: string;
  state: ServiceState;
  latency_ms: number | null;
  http_status: number | null;
}

export interface RailStatusDeps {
  services: readonly RailService[];
  fetchImpl?: typeof fetch;
  /** Per-service request timeout; a service that never answers reads "down", not hung. */
  timeoutMs?: number;
  now?: () => Date;
}

export type Overall = "ok" | "degraded" | "down" | "unconfigured";

export interface RailStatusResult {
  checked_at: string;
  overall: Overall;
  services: ServiceStatus[];
}

const DEFAULT_TIMEOUT_MS = 3000;

async function checkOne(fetchImpl: typeof fetch, timeoutMs: number, svc: RailService): Promise<ServiceStatus> {
  if (!svc.url) return { service: svc.name, state: "not_configured", latency_ms: null, http_status: null };
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const started = Date.now();
  try {
    const res = await fetchImpl(svc.url, { signal: controller.signal });
    return { service: svc.name, state: res.ok ? "ok" : "down", latency_ms: Date.now() - started, http_status: res.status };
  } catch {
    // Network error, DNS failure, connection refused or our own abort on
    // timeout: all read the same to a caller deciding whether to route here.
    return { service: svc.name, state: "down", latency_ms: Date.now() - started, http_status: null };
  } finally {
    clearTimeout(timer);
  }
}

function overallOf(services: readonly ServiceStatus[]): Overall {
  const configured = services.filter((s) => s.state !== "not_configured");
  if (configured.length === 0) return "unconfigured";
  const okCount = configured.filter((s) => s.state === "ok").length;
  if (okCount === configured.length) return "ok";
  if (okCount === 0) return "down";
  return "degraded";
}

/** Check every service in parallel; never throws (a single bad host must not break the aggregate). */
export async function checkRailStatus(deps: RailStatusDeps): Promise<RailStatusResult> {
  const fetchImpl = deps.fetchImpl ?? fetch;
  const timeoutMs = deps.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const now = deps.now ?? (() => new Date());
  const services = await Promise.all(deps.services.map((s) => checkOne(fetchImpl, timeoutMs, s)));
  return { checked_at: now().toISOString(), overall: overallOf(services), services };
}
