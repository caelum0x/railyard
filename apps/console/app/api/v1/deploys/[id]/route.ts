import { deps } from "../../../../../lib/deps.ts";
import { json } from "../../../../../lib/http.ts";
import { orgForBearer } from "../../../../../lib/tokens.ts";
import { publicDeploy } from "../service.ts";

export async function GET(req: Request, { params }: { params: { id: string } }) {
  const { repo } = deps();
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  const d = await repo.getDeploy(who.orgId, params.id);
  if (!d) return json({ error: "not found" }, 404);
  const app = await repo.getApp(who.orgId, d.app_id);
  return json({ deploy: publicDeploy(d, app?.name ?? "") });
}
