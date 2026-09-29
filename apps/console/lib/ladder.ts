// The Railyard money ladder as pure functions (lane plan A2):
// sign in -> $1 card check, credited back -> 14-day trial with the plan's
// limits -> $19 a month or $190 a year. Pause and one-click cancel at any
// step. Every function returns a new entitlement; nothing is mutated.

import { PLAN } from "./plans.ts";
import type { Entitlement } from "./types.ts";

export type LadderEvent =
  | { kind: "card_check_paid"; paymentId: string; customerId: string | null; at: Date }
  | { kind: "card_check_refunded"; paymentId: string; at: Date }
  | { kind: "subscription_active"; subscriptionId: string; customerId: string | null; interval: "month" | "year"; at: Date }
  | { kind: "subscription_paused"; at: Date }
  | { kind: "subscription_resumed"; at: Date }
  | { kind: "subscription_cancel_scheduled"; on: boolean; at: Date }
  | { kind: "subscription_ended"; at: Date };

export type Effect =
  | { kind: "wallet_credit"; customerId: string; cents: number; idempotencyKey: string; reason: string }
  | { kind: "wallet_debit"; customerId: string; cents: number; idempotencyKey: string; reason: string };

const DAY_MS = 86_400_000;

function withLimits(ent: Entitlement, on: boolean): Entitlement {
  return { ...ent, apps_limit: on ? PLAN.appsLimit : 0, events_limit: on ? PLAN.eventsLimit : 0 };
}

export function applyLadder(ent: Entitlement, ev: LadderEvent): { ent: Entitlement; effects: Effect[] } {
  const at = ev.at.toISOString();
  const base = { ...ent, updated_at: at };
  switch (ev.kind) {
    case "card_check_paid": {
      if (ent.card_check_payment_id === ev.paymentId) return { ent, effects: [] };
      const customerId = ev.customerId ?? ent.dodo_customer_id;
      const subscribed = ent.plan === "railyard";
      const next = {
        ...base,
        dodo_customer_id: customerId,
        card_verified_at: at,
        card_check_payment_id: ev.paymentId,
        credit_cents: ent.credit_cents + PLAN.cardCheckCents,
        plan: subscribed ? ent.plan : ("trial" as const),
        status: subscribed ? ent.status : ("trialing" as const),
        trial_ends_at: subscribed ? ent.trial_ends_at : new Date(ev.at.getTime() + PLAN.trialDays * DAY_MS).toISOString(),
      };
      const effects: Effect[] = customerId
        ? [{ kind: "wallet_credit", customerId, cents: PLAN.cardCheckCents, idempotencyKey: `railyard-card-check-credit:${ev.paymentId}`, reason: "Railyard card check, credited back" }]
        : [];
      return { ent: withLimits(next, true), effects };
    }
    case "card_check_refunded": {
      if (ent.card_check_payment_id !== ev.paymentId) return { ent, effects: [] };
      const next: Entitlement = {
        ...base,
        credit_cents: ent.credit_cents - PLAN.cardCheckCents,
        card_verified_at: null,
        card_check_payment_id: null,
      };
      const effects: Effect[] = ent.dodo_customer_id
        ? [{ kind: "wallet_debit", customerId: ent.dodo_customer_id, cents: PLAN.cardCheckCents, idempotencyKey: `railyard-card-check-debit:${ev.paymentId}`, reason: "Railyard card check refunded" }]
        : [];
      if (ent.plan === "trial") {
        return { ent: withLimits({ ...next, plan: "none", status: "inactive", trial_ends_at: null, paused: false }, false), effects };
      }
      return { ent: next, effects };
    }
    case "subscription_active":
      return {
        ent: withLimits(
          {
            ...base,
            plan: "railyard",
            status: "active",
            interval: ev.interval,
            subscription_id: ev.subscriptionId,
            dodo_customer_id: ev.customerId ?? ent.dodo_customer_id,
            paused: false,
            cancel_at_period_end: false,
          },
          true,
        ),
        effects: [],
      };
    case "subscription_paused":
      return { ent: { ...base, paused: true, status: "paused" }, effects: [] };
    case "subscription_resumed":
      return { ent: { ...base, paused: false, status: ent.plan === "railyard" ? "active" : ent.plan === "trial" ? "trialing" : "inactive" }, effects: [] };
    case "subscription_cancel_scheduled":
      return { ent: { ...base, cancel_at_period_end: ev.on }, effects: [] };
    case "subscription_ended":
      return {
        ent: withLimits({ ...base, plan: "none", status: "cancelled", subscription_id: null, interval: null, paused: false, cancel_at_period_end: false }, false),
        effects: [],
      };
  }
}

export interface Access {
  state: "no_card" | "trial" | "trial_ended" | "active" | "paused" | "cancelling" | "cancelled";
  canAddApps: boolean;
  appsLimit: number;
  eventsLimit: number;
  trialDaysLeft: number | null;
}

/** What the org may do right now. A trial past its end date gives no access. */
export function access(ent: Entitlement | null, now: Date): Access {
  if (!ent) return { state: "no_card", canAddApps: false, appsLimit: 0, eventsLimit: 0, trialDaysLeft: null };
  const limits = { appsLimit: ent.apps_limit, eventsLimit: ent.events_limit, trialDaysLeft: null as number | null };
  if (ent.paused) return { state: "paused", canAddApps: false, ...limits };
  if (ent.plan === "railyard") return { state: ent.cancel_at_period_end ? "cancelling" : "active", canAddApps: true, ...limits };
  if (ent.plan === "trial" && ent.trial_ends_at) {
    const left = Math.ceil((Date.parse(ent.trial_ends_at) - now.getTime()) / DAY_MS);
    if (left <= 0) return { state: "trial_ended", canAddApps: false, appsLimit: 0, eventsLimit: 0, trialDaysLeft: 0 };
    return { state: "trial", canAddApps: true, ...limits, trialDaysLeft: left };
  }
  if (ent.status === "cancelled") return { state: "cancelled", canAddApps: false, appsLimit: 0, eventsLimit: 0, trialDaysLeft: null };
  return { state: "no_card", canAddApps: false, appsLimit: 0, eventsLimit: 0, trialDaysLeft: null };
}

/** Local pause or cancel for an org with no subscription (a trial). */
export function localPause(ent: Entitlement, on: boolean, at: Date): Entitlement {
  return applyLadder(ent, on ? { kind: "subscription_paused", at } : { kind: "subscription_resumed", at }).ent;
}

export function endTrial(ent: Entitlement, at: Date): Entitlement {
  return withLimits({ ...ent, plan: "none", status: "cancelled", trial_ends_at: at.toISOString(), paused: false, updated_at: at.toISOString() }, false);
}
