// Mint self-serve account provisioning from the Apps tab (backlog item 11):
// one click instead of an operator curling Mint's `POST /v1/accounts` with
// the master key. Idempotent: calling it again on an app that already has
// an account just returns the existing one.

import { sessionFromRequest } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { json, sameOrigin } from "../../../../../lib/http.ts";

export async function POST(req: Request, { params }: { params: { id: string } }) {
  const { cfg, repo, mint } = deps();
  const s = await sessionFromRequest(req);
  if (!s) return json({ error: "Sign in again." }, 401);
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const app = await repo.getApp(s.org, params.id);
  if (!app) return json({ error: "App not found." }, 404);
  if (app.mint_account_id) return json({ mint_account_id: app.mint_account_id });
  if (!mint) return json({ error: "Billing accounts are not configured yet. Set MINT_URL and MINT_API_KEY." }, 503);
  try {
    const account = await mint.createAccount(app.id);
    const saved = await repo.setAppMintAccount(s.org, app.id, account.accountId);
    return json({ mint_account_id: saved?.mint_account_id ?? account.accountId }, 201);
  } catch (err) {
    console.error(`mint account provisioning failed for app ${app.id}: ${(err as Error).message}`);
    return json({ error: "Could not reach Mint. Try again." }, 502);
  }
}
