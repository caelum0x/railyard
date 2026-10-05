import { BillingError, startCheckout, type CheckoutKind } from "../../../../lib/billing.ts";
import { sessionFromRequest } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { sameOrigin } from "../../../../lib/http.ts";

export async function POST(req: Request) {
  const d = deps();
  const s = await sessionFromRequest(req);
  if (!s) return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/` } });
  if (!sameOrigin(req, d.cfg.consoleUrl)) return new Response("Forbidden", { status: 403 });
  if (!d.cfg.billingReady) return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/billing?error=arriving` } });
  const form = await req.formData();
  const kind = form.get("kind") === "plan" ? "plan" : ("card_check" as CheckoutKind);
  const interval = form.get("interval") === "year" ? "year" : "month";
  try {
    const url = await startCheckout(d, { orgId: s.org, email: s.email }, kind, interval);
    return new Response(null, { status: 303, headers: { location: url } });
  } catch (err) {
    if (err instanceof BillingError) return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/billing?error=already` } });
    console.error(`checkout failed for org ${s.org}: ${(err as Error).message}`);
    return new Response(null, { status: 303, headers: { location: `${d.cfg.consoleUrl}/billing?error=checkout` } });
  }
}
