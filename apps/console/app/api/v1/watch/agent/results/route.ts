import { deps } from "../../../../../../lib/deps.ts";
import { agentResults } from "../../../../../../lib/watch/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  return agentResults(deps().watch, req, new Date());
}
