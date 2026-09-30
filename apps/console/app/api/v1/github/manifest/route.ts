// Ship on push (PY3.1): the owner creates the Railyard GitHub App from this manifest (owner item Y4).
import { deps } from "../../../../../lib/deps.ts";
import { manifestPage } from "../../../../../lib/ship/manifest.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return manifestPage(deps().cfg.consoleUrl, new URL(req.url).searchParams.get("org"));
}
