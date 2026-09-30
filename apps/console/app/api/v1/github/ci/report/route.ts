// Ship on push (PY3.1): the workflow reports the deploy (and a preview goes on its PR).
import { deps } from "../../../../../../lib/deps.ts";
import { ciReportH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  return ciReportH(deps().ship, req, new Date());
}
