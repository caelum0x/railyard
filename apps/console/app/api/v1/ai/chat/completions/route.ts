import { chatCompletion } from "../../../../../../lib/ai/gateway.ts";
import { deps } from "../../../../../../lib/deps.ts";
import { json } from "../../../../../../lib/http.ts";

export const dynamic = "force-dynamic";

// OpenAI-compatible chat completions for Scribase projects, BYOK only (PY1.4).
export async function POST(req: Request) {
  const { cfg, repo } = deps();
  if (!cfg.aiGateway) return json({ error: { code: "gateway_not_configured", message: "The AI gateway is not set up on this deployment yet." } }, 503);
  const r = await chatCompletion(
    { repo, ...cfg.aiGateway, consoleUrl: cfg.consoleUrl, now: () => new Date() },
    req.headers.get("authorization"),
    await req.text(),
  );
  return json(r.body, r.status, r.headers ?? {});
}
