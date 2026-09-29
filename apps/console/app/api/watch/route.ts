import { sessionFromRequest } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { json, sameOrigin } from "../../../lib/http.ts";
import { consoleWatch } from "../../../lib/watch/http.ts";

// Add, pause or resume an uptime check from the Watch screen (PY2.2).
export async function POST(req: Request) {
  const { cfg, repo, watch } = deps();
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const r = await consoleWatch(watch, repo, await sessionFromRequest(req), await req.json().catch(() => null));
  return json(r.body, r.status);
}
