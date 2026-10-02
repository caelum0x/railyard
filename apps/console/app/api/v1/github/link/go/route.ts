// Ship on push (PY3.1): the browser opens the link from the terminal.
import { deps } from "../../../../../../lib/deps.ts";
import { goH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return goH(deps().ship, req, new Date());
}

/** The form that names the org posts back here before any GitHub grant (P-Y10). */
export async function POST(req: Request) {
  return goH(deps().ship, req, new Date());
}
