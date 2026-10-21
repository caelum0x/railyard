// GET /api/v1/rail/status: aggregated health of the money rail's own
// services (Mint, Rago, Relay, Abacus) in one call, instead of making a
// consumer product (Rally, Menivor, Scribase, ...) or an operator probe four
// hosts directly before routing real traffic at the rail. Public and
// read-only -- no secrets in the response, only up/down + latency per
// service -- so any consumer can poll it without a Railyard session.
import { deps } from "../../../../../lib/deps.ts";
import { json } from "../../../../../lib/http.ts";
import { checkRailStatus } from "../../../../../lib/rail-status.ts";

export const dynamic = "force-dynamic";

export async function GET() {
  const { cfg } = deps();
  const result = await checkRailStatus({
    services: [
      { name: "mint", url: cfg.railStatus.mint },
      { name: "rago", url: cfg.railStatus.rago },
      { name: "relay", url: cfg.railStatus.relay },
      { name: "abacus", url: cfg.railStatus.abacus },
    ],
  });
  // ok/degraded answer 200 (the body carries the detail); down/unconfigured
  // answer 503 so a naive caller that only checks the HTTP status still
  // reads the rail as unavailable without having to parse the body.
  const status = result.overall === "ok" || result.overall === "degraded" ? 200 : 503;
  return json(result, status);
}
