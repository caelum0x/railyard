import { sessionFromRequest } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { consoleSegment } from "../../../../lib/flags/http.ts";
import { json, sameOrigin } from "../../../../lib/http.ts";

// Create, change or remove one segment from the Flags screen (QY1.3).
export async function POST(req: Request) {
  const { cfg, flags } = deps();
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const r = await consoleSegment(flags, await sessionFromRequest(req), await req.json().catch(() => null), new Date());
  return json(r.body, r.status);
}
