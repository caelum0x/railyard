import { deps } from "../../../../../lib/deps.ts";
import { json } from "../../../../../lib/http.ts";
import { pollDevice } from "../../../../../lib/tokens.ts";

export async function POST(req: Request) {
  const { repo } = deps();
  const body = (await req.json().catch(() => ({}))) as { device_code?: unknown };
  if (typeof body.device_code !== "string") return json({ error: "invalid_request" }, 400);
  const r = await pollDevice(repo, body.device_code, new Date());
  return json(r.body, r.status);
}
