import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { logger } from "~/utils/logger";

/**
 * UI readiness: healthy only when the Relay gateway it fronts is ready.
 *
 * The gateway's own `/health` composes Postgres/ClickHouse/Valkey probes with
 * the Relay catalog and (when enabled) the Mint billing backend, so a green UI
 * health check means the product is actually usable. Add `?live=1` for a
 * process-only liveness probe that never touches the gateway.
 */
export async function loader({ request }: { request: Request }) {
  const url = new URL(request.url);
  if (url.searchParams.get("live") === "1") {
    return Response.json({ ui: "ok" }, { status: 200 });
  }
  try {
    const gateway = await getTensorZeroClient().gatewayHealth();
    let detail: unknown = gateway.body;
    try {
      detail = JSON.parse(gateway.body);
    } catch {
      // Non-JSON health body; report it verbatim.
    }
    return Response.json(
      { ui: "ok", gateway: gateway.ok ? "ok" : "error", detail },
      { status: gateway.ok ? 200 : 503 },
    );
  } catch (error) {
    logger.error("Relay gateway health check failed", error);
    return Response.json(
      { ui: "ok", gateway: "unreachable" },
      { status: 503 },
    );
  }
}
