// Ship on push (PY3.1): GitHub's redirect after the user signed in with the App
// (the check that the person linking can push to the repository).
import { deps } from "../../../../../../lib/deps.ts";
import { githubUserCallbackH } from "../../../../../../lib/ship/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  return githubUserCallbackH(deps().ship, req, new Date());
}
