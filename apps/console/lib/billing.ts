// Billing actions behind the Billing screen: the $1 card check, the plan
// checkout (monthly or yearly), the paused switch and one-click cancel.

import type { Config } from "./config.ts";
import { Dodo, signMetadata } from "./dodo.ts";
import { access, endTrial, localPause } from "./ladder.ts";
import { randomToken } from "./crypto.ts";
import type { Repo } from "./repo.ts";
import { emptyEntitlement, type Entitlement } from "./types.ts";
import type { Interval } from "./plans.ts";

export type CheckoutKind = "card_check" | "plan";

export class BillingError extends Error {
  code: "already_subscribed" | "card_on_file";
  constructor(code: "already_subscribed" | "card_on_file", message: string) {
    super(message);
    this.code = code;
  }
}

export async function startCheckout(
  deps: { repo: Repo; dodo: Pick<Dodo, "createCheckout">; cfg: Pick<Config, "consoleUrl" | "sessionSecret" | "products"> },
  who: { orgId: string; email: string },
  kind: CheckoutKind,
  interval: Interval = "month",
): Promise<string> {
  // Never open a second charge for something the org already has: a second
  // subscription would bill twice, a second card check could restart a trial.
  const ent = await deps.repo.getEntitlement(who.orgId);
  if (kind === "plan" && ent?.plan === "railyard" && ent.subscription_id) throw new BillingError("already_subscribed", "This org already has the Railyard plan.");
  if (kind === "card_check" && ent?.card_verified_at) throw new BillingError("card_on_file", "A card is already on file.");
  const requestId = randomToken(12);
  const productId = kind === "card_check" ? deps.cfg.products.cardCheck : interval === "year" ? deps.cfg.products.year : deps.cfg.products.month;
  const metadata: Record<string, string> = {
    service: "railyard",
    org_id: who.orgId,
    kind,
    request_id: requestId,
    sig: await signMetadata(deps.cfg.sessionSecret, who.orgId, kind, requestId),
  };
  if (kind === "plan") metadata.interval = interval;
  const { checkoutUrl } = await deps.dodo.createCheckout({
    productId,
    email: who.email,
    metadata,
    returnUrl: `${deps.cfg.consoleUrl}/billing?checkout=${kind}`,
  });
  return checkoutUrl;
}

async function current(repo: Repo, orgId: string, now: Date): Promise<Entitlement> {
  return (await repo.getEntitlement(orgId)) ?? emptyEntitlement(orgId, now);
}

/** The paused switch. A subscription is paused at the provider; a trial is paused here. */
export async function setPaused(deps: { repo: Repo; dodo: Pick<Dodo, "setPaused"> }, orgId: string, paused: boolean, now: Date): Promise<Entitlement> {
  const ent = await current(deps.repo, orgId, now);
  if (ent.subscription_id) await deps.dodo.setPaused(ent.subscription_id, paused);
  return deps.repo.saveEntitlement(localPause(ent, paused, now));
}

/** One click, no survey. A subscription ends at its next billing date; a trial ends now. */
export async function cancel(deps: { repo: Repo; dodo: Pick<Dodo, "cancelAtPeriodEnd"> }, orgId: string, now: Date): Promise<Entitlement> {
  const ent = await current(deps.repo, orgId, now);
  if (ent.subscription_id && ent.plan === "railyard") {
    await deps.dodo.cancelAtPeriodEnd(ent.subscription_id);
    return deps.repo.saveEntitlement({ ...ent, cancel_at_period_end: true, updated_at: now.toISOString() });
  }
  if (access(ent, now).state === "trial" || ent.plan === "trial") return deps.repo.saveEntitlement(endTrial(ent, now));
  return ent;
}
