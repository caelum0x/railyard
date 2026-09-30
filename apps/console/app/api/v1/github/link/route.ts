// Ship on push (PY3.1): `railyard link` starts a link (CLI bearer).
import { deps } from "../../../../../lib/deps.ts";
import { linkStartH } from "../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const d = deps();
  return linkStartH(d.ship, d.repo, req, new Date());
}
