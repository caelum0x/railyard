import type { Route } from "./+types/route";
import { Suspense } from "react";
import { Await, useAsyncError, useLocation, type RouteHandle } from "react-router";
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
import { Skeleton } from "~/components/ui/skeleton";
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { BillingSummary } from "~/types/billing";

const MICROS_PER_USD = 1_000_000;
const SETTLEMENT_HISTORY_LIMIT = 50;

export const handle: RouteHandle = {
  crumb: () => ["Billing"],
};

export async function loader() {
  await requireValidApiKeyIfEnabled();

  // Deferred: the summary is aggregated by the gateway from the durable
  // cost-event and settlement-evidence tables and streamed in via `Await`.
  const summary = getTensorZeroClient().getBillingSummary({
    settlements: SETTLEMENT_HISTORY_LIMIT,
  });

  return { summary };
}

function formatUsd(micros: number): string {
  return (micros / MICROS_PER_USD).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

function formatCount(value: number): string {
  return value.toLocaleString();
}

function formatTimestamp(ms: number | undefined): string {
  if (ms == null) {
    return "—";
  }
  return new Date(ms).toLocaleString();
}

function BillingPageHeader() {
  return <PageHeader heading="Billing" />;
}

function StatCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="border-border bg-bg-secondary flex flex-col gap-1 rounded-md border p-4">
      <span className="text-fg-tertiary text-xs font-medium tracking-wide uppercase">
        {label}
      </span>
      <span className="text-fg-primary text-xl font-medium">{value}</span>
    </div>
  );
}

function BillingSkeleton() {
  return (
    <>
      <BillingPageHeader />
      <SectionLayout>
        <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
          {[1, 2, 3, 4, 5, 6].map((i) => (
            <Skeleton key={i} className="h-20 w-full rounded-md" />
          ))}
        </div>
        <Skeleton className="h-48 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function BillingErrorState() {
  const error = useAsyncError();
  return (
    <>
      <BillingPageHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

function EmptyRow({ colSpan, message }: { colSpan: number; message: string }) {
  return (
    <TableRow>
      <TableCell colSpan={colSpan} className="text-fg-muted text-center">
        {message}
      </TableCell>
    </TableRow>
  );
}

function BillingContent({ summary }: { summary: BillingSummary }) {
  const { wallet, totals, by_key, by_model, recent_settlements } = summary;

  return (
    <>
      <BillingPageHeader />
      <SectionsGroup>
        <SectionLayout>
          <SectionHeader heading="Wallet" />
          <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
            <StatCard
              label="Total charged"
              value={formatUsd(wallet.total_charged_micros)}
            />
            <StatCard
              label="Total settled"
              value={formatUsd(wallet.total_settled_micros)}
            />
            <StatCard
              label="Total authorized"
              value={formatUsd(wallet.total_authorized_micros)}
            />
            <StatCard
              label="Cost events"
              value={formatCount(wallet.cost_event_count)}
            />
            <StatCard
              label="Settlements"
              value={formatCount(wallet.settlement_count)}
            />
            <StatCard
              label="Last settlement"
              value={formatTimestamp(wallet.last_settled_at_ms)}
            />
          </div>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Usage totals" />
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
            <StatCard
              label="Input tokens"
              value={formatCount(totals.input_tokens)}
            />
            <StatCard
              label="Output tokens"
              value={formatCount(totals.output_tokens)}
            />
            <StatCard
              label="Total tokens"
              value={formatCount(totals.total_tokens)}
            />
            <StatCard
              label="Charged"
              value={formatUsd(totals.charged_micros)}
            />
          </div>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Spend by API key" />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>API key</TableHead>
                <TableHead className="text-right">Cost events</TableHead>
                <TableHead className="text-right">Charged</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {by_key.length === 0 ? (
                <EmptyRow colSpan={3} message="No spend recorded yet." />
              ) : (
                by_key.map((row) => (
                  <TableRow key={row.key}>
                    <TableCell className="font-mono">{row.key}</TableCell>
                    <TableCell className="text-right">
                      {formatCount(row.cost_event_count)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(row.charged_micros)}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Spend by model" />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Model</TableHead>
                <TableHead className="text-right">Cost events</TableHead>
                <TableHead className="text-right">Tokens</TableHead>
                <TableHead className="text-right">Charged</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {by_model.length === 0 ? (
                <EmptyRow colSpan={4} message="No spend recorded yet." />
              ) : (
                by_model.map((row) => (
                  <TableRow key={row.model}>
                    <TableCell className="font-mono">{row.model}</TableCell>
                    <TableCell className="text-right">
                      {formatCount(row.cost_event_count)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatCount(row.total_tokens)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(row.charged_micros)}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Settlement history" />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Request ID</TableHead>
                <TableHead className="text-right">Amount</TableHead>
                <TableHead className="text-right">Settled</TableHead>
                <TableHead className="text-right">Verified</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {recent_settlements.length === 0 ? (
                <EmptyRow colSpan={4} message="No settlements recorded yet." />
              ) : (
                recent_settlements.map((row) => (
                  <TableRow key={row.request_id}>
                    <TableCell className="font-mono">{row.request_id}</TableCell>
                    <TableCell className="text-right">
                      {formatUsd(row.amount_micros)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTimestamp(row.settled_at_ms)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTimestamp(row.verified_at_ms)}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>
      </SectionsGroup>
    </>
  );
}

export default function BillingPage({ loaderData }: Route.ComponentProps) {
  const { summary } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<BillingSkeleton />}>
        <Await resolve={summary} errorElement={<BillingErrorState />}>
          {(resolved) => <BillingContent summary={resolved} />}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
