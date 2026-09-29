import { deps } from "../../../../../lib/deps.ts";
import { clientFlags } from "../../../../../lib/flags/http.ts";

export const dynamic = "force-dynamic";

// What an app reads with its public flags key (PY2.1). CORS open; cached 15 s.
export async function GET(req: Request) {
  return clientFlags(deps().flags, req, new Date());
}

export async function OPTIONS(req: Request) {
  return clientFlags(deps().flags, req, new Date());
}
