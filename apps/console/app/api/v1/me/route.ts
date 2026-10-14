import { deps } from "../../../../lib/deps.ts";
import { json } from "../../../../lib/http.ts";
import { access } from "../../../../lib/ladder.ts";
import { orgForBearer } from "../../../../lib/tokens.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const { repo } = deps();
  const who = await orgForBearer(repo, req.headers.get("authorization"));
  if (!who) return json({ error: "unauthorized" }, 401);
  const org = await repo.getOrg(who.orgId);
  const a = access(await repo.getEntitlement(who.orgId), new Date());
  const apps = await repo.listApps(who.orgId);
  return json({ org: { id: org?.id, name: org?.name }, access: a, apps: apps.map((x) => ({ id: x.id, name: x.name })) });
}
