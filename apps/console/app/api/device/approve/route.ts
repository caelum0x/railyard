import { sessionFromRequest } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { sameOrigin } from "../../../../lib/http.ts";
import { approveDevice } from "../../../../lib/tokens.ts";

export async function POST(req: Request) {
  const { cfg, repo } = deps();
  const s = await sessionFromRequest(req);
  if (!s) return new Response(null, { status: 303, headers: { location: `${cfg.consoleUrl}/` } });
  if (!sameOrigin(req, cfg.consoleUrl)) return new Response("Forbidden", { status: 403 });
  const code = String((await req.formData()).get("code") ?? "");
  const ok = await approveDevice(repo, code, s.org, s.uid, new Date());
  return new Response(null, { status: 303, headers: { location: `${cfg.consoleUrl}/device?${ok ? "approved=1" : "error=code"}` } });
}
