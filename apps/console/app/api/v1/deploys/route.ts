import { deps } from "../../../../lib/deps.ts";
import { json } from "../../../../lib/http.ts";
import { orgForBearer } from "../../../../lib/tokens.ts";
import { listDeploys, parseDeployInput, recordDeploy } from "./service.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const { repo } = deps();
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  const input = parseDeployInput(await req.json().catch(() => null));
  if (typeof input === "string") return json({ error: input }, 400);
  const r = await recordDeploy(repo, who.orgId, input, new Date());
  return json(r.body, r.status);
}

export async function GET(req: Request) {
  const { repo } = deps();
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  const q = new URL(req.url).searchParams;
  const r = await listDeploys(repo, who.orgId, q.get("app"), q.get("branch"));
  return json(r.body, r.status);
}
