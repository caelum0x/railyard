// Ship on push (PY3.1): GitHub's Setup URL after the app is installed.
import { deps } from "../../../../../lib/deps.ts";
import { setupH } from "../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return setupH(deps().ship, req, new Date());
}
