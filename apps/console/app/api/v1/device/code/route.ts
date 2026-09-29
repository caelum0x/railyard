import { deps } from "../../../../../lib/deps.ts";
import { json } from "../../../../../lib/http.ts";
import { startDevice } from "../../../../../lib/tokens.ts";

export async function POST() {
  const { cfg, repo } = deps();
  return json(await startDevice(repo, cfg.consoleUrl, new Date()));
}
