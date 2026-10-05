// What `railyard domains` did with the local Cloudflare login (QY1.5).
import { deps } from "../../../../../lib/deps.ts";
import { domainsRecordH } from "../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const d = deps();
  return domainsRecordH(d.ship, d.repo, req, new Date());
}
