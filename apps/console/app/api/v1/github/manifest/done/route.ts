// Ship on push (PY3.1): GitHub's redirect after the App is created from the manifest.
import { manifestDone } from "../../../../../../lib/ship/manifest.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return manifestDone(new URL(req.url).searchParams.get("code"));
}
