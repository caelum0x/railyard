// Ship on push (PY3.1): GitHub App events (uninstalls and removed repositories).
import { deps } from "../../../../../lib/deps.ts";
import { webhookH } from "../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const d = deps();
  return webhookH(d.ship, d.cfg.ship?.github.webhookSecret ?? null, req);
}
