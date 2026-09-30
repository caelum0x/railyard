import { deps } from "../../../../lib/deps.ts";
import { tokenListFlags } from "../../../../lib/flags/http.ts";
import { json } from "../../../../lib/http.ts";
import { orgForBearer } from "../../../../lib/tokens.ts";

export const dynamic = "force-dynamic";

// The org's flags for its own `ry_` token, read only (the agent surface, PY3.4).
export async function GET(req: Request) {
  const { repo, flags } = deps();
  const r = await tokenListFlags(flags, await orgForBearer(repo, req.headers.get("authorization")), new URL(req.url).searchParams.get("app"));
  return json(r.body, r.status);
}
