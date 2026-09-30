// Ship on push (PY3.1): the workflow exchanges its GitHub OIDC token.
import { deps } from "../../../../../../lib/deps.ts";
import { ciSessionH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  return ciSessionH(deps().ship, req, new Date());
}
