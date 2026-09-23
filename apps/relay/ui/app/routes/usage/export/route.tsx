import type { Route } from "./+types/route";
import { useMemo, useState } from "react";
import type { RouteHandle } from "react-router";
import {
  PageHeader,
  PageLayout,
  SectionHeader,
  SectionLayout,
  SectionsGroup,
} from "~/components/layout/PageLayout";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "~/components/ui/table";
import { Button } from "~/components/ui/button";
import { Input } from "~/components/ui/input";
import { LayoutErrorBoundary } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { BillingStatus, KeySpendPoint } from "~/types/billing";
import { ScopeToggle, scopeFromUrl } from "~/components/billing/BillingShared";

const MICROS_PER_USD = 1_000_000;
const DEFAULT_RANGE_DAYS = 30;

export const handle: RouteHandle = {
  crumb: () => ["Usage", "Export"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope = scopeFromUrl(request.url);
  const client = getTensorZeroClient();
  // Both the aggregated summary and the per-day key-spend series feed the
  // client-side CSV export; the summary carries the model/wallet rollup while
  // the series is what the selected date range filters.
  const [summary, keySpend, status] = await Promise.all([
    client.getBillingSummary({ settlements: 500, scope }),
    client.getBillingKeySpend({ scope }),
    client.getBillingStatus().catch((): BillingStatus | null => null),
  ]);
  return { summary, keySpend, status, scope };
}

function toUsd(micros: number): number {
  return micros / MICROS_PER_USD;
}

function formatUsd(micros: number): string {
  return toUsd(micros).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

function formatDay(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** Local `YYYY-MM-DD` for a Date, suitable for a date input value. */
function toDateInputValue(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/** Inclusive start-of-day epoch ms for a `YYYY-MM-DD` string, or null. */
function startOfDayMs(value: string): number | null {
  if (!value) return null;
  const parsed = new Date(`${value}T00:00:00`);
  const ms = parsed.getTime();
  return Number.isNaN(ms) ? null : ms;
}

/** Exclusive end-of-day epoch ms (start of the next day) for a date string. */
function endOfDayMs(value: string): number | null {
  const start = startOfDayMs(value);
  if (start == null) return null;
  return start + 24 * 60 * 60 * 1000;
}

function escapeCsv(field: string | number): string {
  const text = String(field);
  if (/[",\n\r]/.test(text)) {
    return `"${text.replace(/"/g, '""')}"`;
  }
  return text;
}

function toCsv(headers: string[], rows: (string | number)[][]): string {
  const lines = [headers, ...rows].map((row) => row.map(escapeCsv).join(","));
  return lines.join("\r\n");
}

function downloadCsv(filename: string, csv: string): void {
  const blob = new Blob([csv], { type: "text/csv;charset=utf-8;" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

function pointInRange(
  point: KeySpendPoint,
  fromMs: number | null,
  toMs: number | null,
): boolean {
  if (fromMs != null && point.bucket_start_ms < fromMs) return false;
  if (toMs != null && point.bucket_start_ms >= toMs) return false;
  return true;
}

export default function UsageExportPage({ loaderData }: Route.ComponentProps) {
  const { summary, keySpend, status, scope } = loaderData;

  const now = useMemo(() => new Date(), []);
  const defaultFrom = useMemo(() => {
    const date = new Date(now);
    date.setDate(date.getDate() - DEFAULT_RANGE_DAYS);
    return date;
  }, [now]);

  const [from, setFrom] = useState(() => toDateInputValue(defaultFrom));
  const [to, setTo] = useState(() => toDateInputValue(now));

  const fromMs = startOfDayMs(from);
  const toMs = endOfDayMs(to);
  const rangeValid = fromMs == null || toMs == null || fromMs < toMs;

  const rangePoints = useMemo(() => {
    if (!rangeValid) return [];
    return [...keySpend.points]
      .filter((point) => pointInRange(point, fromMs, toMs))
      .sort((left, right) => right.bucket_start_ms - left.bucket_start_ms);
  }, [keySpend.points, fromMs, toMs, rangeValid]);

  const rangeTotals = useMemo(() => {
    return rangePoints.reduce(
      (acc, point) => ({
        charged_micros: acc.charged_micros + point.charged_micros,
        total_tokens: acc.total_tokens + point.total_tokens,
        cost_event_count: acc.cost_event_count + point.cost_event_count,
      }),
      { charged_micros: 0, total_tokens: 0, cost_event_count: 0 },
    );
  }, [rangePoints]);

  const rangeLabel = `${from || "start"}_to_${to || "now"}`;
  const eventsExportHref = (() => {
    const params = new URLSearchParams();
    if (from) params.set("from", from);
    if (to) params.set("to", to);
    if (scope === "all") params.set("scope", "all");
    return `/usage/export/events.csv?${params.toString()}`;
  })();

  const handleDailyExport = () => {
    const csv = toCsv(
      ["date", "day_start_ms", "api_key", "cost_events", "total_tokens", "charged_usd"],
      rangePoints.map((point) => [
        formatDay(point.bucket_start_ms),
        point.bucket_start_ms,
        point.key,
        point.cost_event_count,
        point.total_tokens,
        toUsd(point.charged_micros).toFixed(6),
      ]),
    );
    downloadCsv(`relay_daily_cost_${rangeLabel}.csv`, csv);
  };

  const handleModelExport = () => {
    const csv = toCsv(
      ["model", "cost_events", "total_tokens", "charged_usd"],
      summary.by_model.map((row) => [
        row.model,
        row.cost_event_count,
        row.total_tokens,
        toUsd(row.charged_micros).toFixed(6),
      ]),
    );
    downloadCsv(`relay_model_spend_${rangeLabel}.csv`, csv);
  };

  const handleSettlementsExport = () => {
    const rows = summary.recent_settlements
      .filter((settlement) => {
        if (fromMs != null && settlement.settled_at_ms < fromMs) return false;
        if (toMs != null && settlement.settled_at_ms >= toMs) return false;
        return true;
      })
      .map((settlement) => [
        settlement.request_id,
        new Date(settlement.settled_at_ms).toISOString(),
        toUsd(settlement.amount_micros).toFixed(6),
        settlement.verified_at_ms != null
          ? new Date(settlement.verified_at_ms).toISOString()
          : "",
      ]);
    const csv = toCsv(
      ["request_id", "settled_at", "amount_usd", "verified_at"],
      rows,
    );
    downloadCsv(`relay_settlements_${rangeLabel}.csv`, csv);
  };

  return (
    <PageLayout>
      <PageHeader heading="Usage & Cost Export" />
      <ScopeToggle scope={scope} operator={status?.operator ?? false} />
      <SectionsGroup>
        <SectionLayout>
          <SectionHeader heading="Date range" />
          <div className="flex flex-wrap items-end gap-4">
            <label className="flex flex-col gap-1">
              <span className="text-fg-tertiary text-xs font-medium uppercase">
                From
              </span>
              <Input
                type="date"
                value={from}
                max={to}
                onChange={(event) => setFrom(event.target.value)}
                className="w-44"
              />
            </label>
            <label className="flex flex-col gap-1">
              <span className="text-fg-tertiary text-xs font-medium uppercase">
                To
              </span>
              <Input
                type="date"
                value={to}
                min={from}
                onChange={(event) => setTo(event.target.value)}
                className="w-44"
              />
            </label>
          </div>
          {!rangeValid && (
            <p className="text-destructive text-sm">
              The start date must be on or before the end date.
            </p>
          )}
          <p className="text-fg-tertiary text-sm">
            The per-request CSV is generated on the server from every durable
            cost event in the range (UTC days), including postpaid metered
            usage. Daily and model CSVs use the aggregated spend rollups;
            settlements cover the most recent 500 Mint receipts.
          </p>
          <div className="flex flex-wrap gap-2">
            {rangeValid ? (
              <Button asChild>
                <a href={eventsExportHref} download>
                  Download per-request CSV
                </a>
              </Button>
            ) : (
              <Button disabled>Download per-request CSV</Button>
            )}
            <Button
              variant="outline"
              onClick={handleDailyExport}
              disabled={!rangeValid || rangePoints.length === 0}
            >
              Download daily cost CSV
            </Button>
            <Button
              variant="outline"
              onClick={handleModelExport}
              disabled={summary.by_model.length === 0}
            >
              Download model spend CSV
            </Button>
            <Button
              variant="outline"
              onClick={handleSettlementsExport}
              disabled={!rangeValid}
            >
              Download settlements CSV
            </Button>
          </div>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Preview: daily cost in range" />
          <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
            <div className="border-border bg-bg-secondary flex flex-col gap-1 rounded-md border p-4">
              <span className="text-fg-tertiary text-xs font-medium uppercase">
                Charged
              </span>
              <span className="text-fg-primary text-xl font-medium">
                {formatUsd(rangeTotals.charged_micros)}
              </span>
            </div>
            <div className="border-border bg-bg-secondary flex flex-col gap-1 rounded-md border p-4">
              <span className="text-fg-tertiary text-xs font-medium uppercase">
                Tokens
              </span>
              <span className="text-fg-primary text-xl font-medium">
                {rangeTotals.total_tokens.toLocaleString()}
              </span>
            </div>
            <div className="border-border bg-bg-secondary flex flex-col gap-1 rounded-md border p-4">
              <span className="text-fg-tertiary text-xs font-medium uppercase">
                Cost events
              </span>
              <span className="text-fg-primary text-xl font-medium">
                {rangeTotals.cost_event_count.toLocaleString()}
              </span>
            </div>
          </div>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Day</TableHead>
                <TableHead>API key</TableHead>
                <TableHead className="text-right">Cost events</TableHead>
                <TableHead className="text-right">Tokens</TableHead>
                <TableHead className="text-right">Charged</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rangePoints.length === 0 ? (
                <TableRow>
                  <TableCell
                    colSpan={5}
                    className="text-fg-muted text-center"
                  >
                    No spend in the selected range.
                  </TableCell>
                </TableRow>
              ) : (
                rangePoints.map((point) => (
                  <TableRow key={`${point.key}-${point.bucket_start_ms}`}>
                    <TableCell>{formatDay(point.bucket_start_ms)}</TableCell>
                    <TableCell className="font-mono">{point.key}</TableCell>
                    <TableCell className="text-right">
                      {point.cost_event_count.toLocaleString()}
                    </TableCell>
                    <TableCell className="text-right">
                      {point.total_tokens.toLocaleString()}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(point.charged_micros)}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>
      </SectionsGroup>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
