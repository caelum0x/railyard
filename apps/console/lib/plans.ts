// The one Railyard plan (lane plan A2). Every price and cap shown in the
// console is read from here (copy-source: apps/console/lib/plans.ts).

export const PLAN = {
  id: "railyard",
  name: "Railyard",
  monthCents: 1900,
  yearCents: 19000,
  appsLimit: 3,
  eventsLimit: 100_000,
  trialDays: 14,
  cardCheckCents: 100,
} as const;

export type Interval = "month" | "year";

export function priceLabel(interval: Interval): string {
  const cents = interval === "year" ? PLAN.yearCents : PLAN.monthCents;
  return `$${(cents / 100).toFixed(0)} a ${interval}`;
}

export function formatCents(cents: number): string {
  const sign = cents < 0 ? "-" : "";
  return `${sign}$${(Math.abs(cents) / 100).toFixed(2)}`;
}
