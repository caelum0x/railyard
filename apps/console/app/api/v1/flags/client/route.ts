import { deps } from "../../../../../lib/deps.ts";
import { clientFlags, type EdgeCache } from "../../../../../lib/flags/http.ts";

// The Workers edge cache (absent in `next dev`).
const edgeCache = (): EdgeCache | null => (globalThis as unknown as { caches?: { default?: EdgeCache } }).caches?.default ?? null;

export const dynamic = "force-dynamic";

// What an app reads with its public flags key (PY2.1). CORS open; cached 15 s.
export async function GET(req: Request) {
  return clientFlags(deps().flags, req, new Date(), edgeCache());
}

export async function OPTIONS(req: Request) {
  return clientFlags(deps().flags, req, new Date());
}
