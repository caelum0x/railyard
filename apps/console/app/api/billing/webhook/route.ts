import { deps } from "../../../../lib/deps.ts";
import { json } from "../../../../lib/http.ts";
import { handleDodoWebhook } from "../../../../lib/webhook.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const { cfg, repo, dodo } = deps();
  const body = await req.text();
  const res = await handleDodoWebhook(
    { repo, dodo, webhookSecret: cfg.dodoWebhookSecret, sessionSecret: cfg.sessionSecret, brandId: cfg.dodoBrandId, now: () => new Date() },
    { id: req.headers.get("webhook-id"), timestamp: req.headers.get("webhook-timestamp"), signature: req.headers.get("webhook-signature") },
    body,
  );
  return json(res.body, res.status);
}
