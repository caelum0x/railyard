// Custom domains for shipped apps (QY1.5): CLI bearer; the Cloudflare grant stays in the console.
import { deps } from "../../../../lib/deps.ts";
import { domainsH } from "../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

const handle = (req: Request) => {
  const d = deps();
  return domainsH(d.ship, d.repo, req, new Date());
};

export const GET = handle;
export const POST = handle;
export const DELETE = handle;
