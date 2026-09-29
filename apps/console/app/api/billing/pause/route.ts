import { setPaused } from "../../../../lib/billing.ts";
import { sessionFromRequest } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { sameOrigin } from "../../../../lib/http.ts";

export async function POST(req: Request) {
  const d = deps();
  const s = await sessionFromRequest(req);
  if (!s) return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/` } });
  if (!sameOrigin(req, d.cfg.consoleUrl)) return new Response("Forbidden", { status: 403 });
  const paused = (await req.formData()).get("paused") === "1";
  try {
    await setPaused(d, s.org, paused, new Date());
    return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/billing` } });
  } catch (err) {
    console.error(`pause failed for org ${s.org}: ${(err as Error).message}`);
    return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/billing?error=pause` } });
  }
}
