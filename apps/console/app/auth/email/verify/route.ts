import { deps } from "../../../../lib/deps.ts";
import { verifyEmailSignIn } from "../../../../lib/email-signin.ts";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const { cfg, repo } = deps();
  return verifyEmailSignIn(req, { cfg, repo });
}
