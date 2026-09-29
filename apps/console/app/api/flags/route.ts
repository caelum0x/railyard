import { sessionFromRequest } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { consoleSetFlag } from "../../../lib/flags/http.ts";
import { json, sameOrigin } from "../../../lib/http.ts";

// Create or change one flag from the Flags screen (PY2.1).
export async function POST(req: Request) {
  const { cfg, repo, flags } = deps();
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const r = await consoleSetFlag(flags, repo, await sessionFromRequest(req), await req.json().catch(() => null), new Date());
  return json(r.body, r.status);
}
