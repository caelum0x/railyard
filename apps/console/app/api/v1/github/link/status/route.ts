// Ship on push (PY3.1): `railyard link` polls until the browser steps are done.
import { deps } from "../../../../../../lib/deps.ts";
import { linkStatusH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const d = deps();
  return linkStatusH(d.ship, d.repo, req);
}
