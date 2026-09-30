// Ship on push (PY3.1): Cloudflare's redirect after the consent screen.
import { deps } from "../../../../../../lib/deps.ts";
import { cloudflareCallbackH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return cloudflareCallbackH(deps().ship, req, new Date());
}
