import { deps } from "../../../../../../lib/deps.ts";
import { json } from "../../../../../../lib/http.ts";
import { handleBridge } from "../service.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const { cfg, repo, sites, insights } = deps();
  if (!cfg.scribaseBridgeSecret) return json({ error: "bridge_not_configured" }, 503);
  const r = await handleBridge(
    { repo, secret: cfg.scribaseBridgeSecret, consoleUrl: cfg.consoleUrl, now: () => new Date(), sites, insights, insightsUrl: cfg.insightsUrl, acceptLegacy: cfg.bridgeAcceptLegacy },
    "orgs",
    { id: req.headers.get("webhook-id"), timestamp: req.headers.get("webhook-timestamp"), signature: req.headers.get("webhook-signature") },
    await req.text(),
  );
  return json(r.body, r.status);
}
