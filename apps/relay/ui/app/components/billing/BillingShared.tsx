/**
 * Shared presentation helpers for Relay's billing pages: money/time
 * formatting, stat cards, the configuration-status banner, and the
 * own-key / all-keys scope toggle.
 */
import { AlertTriangle, CheckCircle2, Info } from "lucide-react";
import { Link, useLocation } from "react-router";
import type { BillingScope, BillingStatus } from "~/types/billing";
import { cn } from "~/utils/common";

export const MICROS_PER_USD = 1_000_000;

export function formatUsd(micros: number): string {
  return (micros / MICROS_PER_USD).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

/**
 * Parse a USD amount like `25` or `12.50` into exact integer micro-USD.
 * Returns `null` for anything that is not a positive amount.
 */
export function parseUsdToMicros(raw: string): number | null {
  const trimmed = raw.trim().replace(/^\$/, "");
  if (!/^\d+(\.\d{1,6})?$/.test(trimmed)) {
    return null;
  }
  const [whole, fraction = ""] = trimmed.split(".");
  const micros =
    Number(whole) * MICROS_PER_USD + Number(fraction.padEnd(6, "0"));
  return Number.isSafeInteger(micros) && micros > 0 ? micros : null;
}

/** Micro-USD rate per million tokens rendered as dollars per million. */
export function formatUsdPerMillion(micros: number): string {
  return `${formatUsd(micros)} / 1M`;
}

export function formatCount(value: number): string {
  return value.toLocaleString();
}

export function formatTimestamp(ms: number | null | undefined): string {
  if (ms == null) {
    return "—";
  }
  return new Date(ms).toLocaleString();
}

/** Parse `?scope=all` from a request URL; anything else is the own scope. */
export function scopeFromUrl(url: string | URL): BillingScope {
  const parsed = typeof url === "string" ? new URL(url) : url;
  return parsed.searchParams.get("scope") === "all" ? "all" : "own";
}

export function StatCard({
  label,
  value,
  hint,
}: {
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <div className="border-border bg-bg-secondary flex flex-col gap-1 rounded-md border p-4">
      <span className="text-fg-tertiary text-xs font-medium tracking-wide uppercase">
        {label}
      </span>
      <span className="text-fg-primary text-xl font-medium">{value}</span>
      {hint && <span className="text-fg-muted text-xs">{hint}</span>}
    </div>
  );
}

function billingModeLabel(status: BillingStatus): string {
  if (status.billing_enabled) {
    return "Prepaid (Mint-settled)";
  }
  if (status.metering_enabled) {
    return "Postpaid metering";
  }
  return "Not recording usage";
}

function costFeedLabel(status: BillingStatus): string {
  const feed = status.cost_feed;
  if (!feed.configured) {
    return "Not forwarding (events stay pending)";
  }
  const target = feed.target_host ?? "configured endpoint";
  if (feed.format === "rago") {
    return `Rago at ${target} (metric ${feed.billable_metric_code ?? "relay_llm_usage"})`;
  }
  return `Webhook at ${target}`;
}

/**
 * Explains how billing is wired on the gateway. Unconfigured deployments show
 * each gap and the env var that closes it rather than silently rendering zeros.
 */
export function BillingStatusBanner({ status }: { status: BillingStatus }) {
  const healthy = status.warnings.length === 0;
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-md border p-4 text-sm",
        healthy
          ? "border-green-200 bg-green-50 text-green-900"
          : "border-amber-200 bg-amber-50 text-amber-900",
      )}
    >
      <div className="flex flex-wrap items-center gap-x-6 gap-y-1">
        <span className="flex items-center gap-2 font-medium">
          {healthy ? (
            <CheckCircle2 className="h-4 w-4" aria-hidden />
          ) : (
            <AlertTriangle className="h-4 w-4" aria-hidden />
          )}
          {billingModeLabel(status)}
        </span>
        <span>Cost feed: {costFeedLabel(status)}</span>
        <span>
          Spend caps: {status.budgets_enforced ? "enforced" : "not enforced"}
        </span>
        {status.principal && (
          <span>
            Viewing as <span className="font-mono">{status.principal}</span>
            {status.operator ? " (operator)" : ""}
          </span>
        )}
      </div>
      {!healthy && (
        <ul className="list-disc space-y-1 pl-6">
          {status.warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Own-key vs all-keys toggle; only rendered for operator keys. */
export function ScopeToggle({
  scope,
  operator,
}: {
  scope: BillingScope;
  operator: boolean;
}) {
  const location = useLocation();
  if (!operator) {
    return null;
  }
  const hrefFor = (next: BillingScope) => {
    const params = new URLSearchParams(location.search);
    if (next === "all") {
      params.set("scope", "all");
    } else {
      params.delete("scope");
    }
    // Cursor pagination is scope-specific.
    params.delete("before");
    const query = params.toString();
    return `${location.pathname}${query ? `?${query}` : ""}`;
  };
  const option = (value: BillingScope, label: string) => (
    <Link
      to={hrefFor(value)}
      preventScrollReset
      aria-current={scope === value ? "page" : undefined}
      className={cn(
        "rounded px-3 py-1 text-sm",
        scope === value
          ? "bg-bg-primary text-fg-primary font-medium shadow-sm"
          : "text-fg-secondary hover:text-fg-primary",
      )}
    >
      {label}
    </Link>
  );
  return (
    <div className="bg-bg-tertiary inline-flex w-fit items-center gap-1 rounded-md p-1">
      {option("own", "This API key")}
      {option("all", "All API keys")}
    </div>
  );
}

export function InlineNotice({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-fg-secondary flex items-start gap-2 text-sm">
      <Info className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
      <span>{children}</span>
    </div>
  );
}
