import { deps } from "../../../../../../lib/deps.ts";
import { agentList } from "../../../../../../lib/watch/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return agentList(deps().watch, req, new Date());
}
