import type { Route } from "./+types/route";
import { Suspense } from "react";
import {
  Await,
  Link,
  useAsyncError,
  useLocation,
  type RouteHandle,
} from "react-router";
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
import type {
  BillingScope,
  BillingStatus,
  BillingSummary,
  WalletBalance,
} from "~/types/billing";
import {
  BillingStatusBanner,
  ScopeToggle,
  StatCard,
  formatCount,
  formatTimestamp,
  formatUsd,
  scopeFromUrl,
} from "~/components/billing/BillingShared";

const SETTLEMENT_HISTORY_LIMIT = 50;

export const handle: RouteHandle = {
  crumb: () => ["Billing"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope = scopeFromUrl(request.url);
  const client = getTensorZeroClient();

  // Deferred: the summary is aggregated by the gateway from the durable
  // cost-event and settlement-evidence tables and streamed in via `Await`.
  // The status explains how billing is wired so an unconfigured gateway is
  // distinguishable from one that simply has no spend yet.
  const status: Promise<BillingStatus | null> = client
    .getBillingStatus()
    .catch(() => null);
  const page = Promise.all([
    client.getBillingSummary({ settlements: SETTLEMENT_HISTORY_LIMIT, scope }),
    status,
    client.getWalletBalance().catch((): WalletBalance | null => null),
  ]);

  return { page, status, scope };
}

function BillingPageHeader() {
  return <PageHeader heading="Billing" />;
}

const BILLING_LINKS = [
  { to: "/billing/keys", label: "API key spend" },
  { to: "/billing/budgets", label: "Spend caps" },
  { to: "/billing/events", label: "Cost events" },
  { to: "/billing/settlements", label: "Settlements" },
  { to: "/usage/export", label: "Usage export" },
] as const;

function BillingToolbar({
  status,
  scope,
}: {
  status: BillingStatus;
  scope: BillingScope;
}) {
  const suffix = scope === "all" ? "?scope=all" : "";
  return (
    <div className="flex flex-col gap-3">
      <BillingStatusBanner status={status} />
      <div className="flex flex-wrap items-center justify-between gap-3">
        <ScopeToggle scope={scope} operator={status.operator} />
        <nav className="flex flex-wrap gap-4 text-sm">
          {BILLING_LINKS.map((link) => (
            <Link
              key={link.to}
              to={`${link.to}${suffix}`}
              className="text-fg-secondary hover:text-fg-primary underline-offset-4 hover:underline"
            >
              {link.label}
            </Link>
          ))}
        </nav>
      </div>
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

function BillingErrorState({
  status,
}: {
  status: Promise<BillingStatus | null>;
}) {
  const error = useAsyncError();
  return (
    <>
      <BillingPageHeader />
      <SectionLayout>
        <Suspense fallback={null}>
          <Await resolve={status} errorElement={null}>
            {(resolved) =>
              resolved ? <BillingStatusBanner status={resolved} /> : null
            }
          </Await>
        </Suspense>
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

function BillingContent({
  summary,
  status,
  scope,
  wallet: walletBalance,
}: {
  summary: BillingSummary;
  status: BillingStatus | null;
  scope: BillingScope;
  wallet: WalletBalance | null;
}) {
  const { wallet, totals, by_key, by_model, recent_settlements } = summary;
  const emptyMessage =
    status && !status.billing_enabled && !status.metering_enabled
      ? "Usage is not being recorded. Enable RELAY_METERING_ENABLED or RELAY_BILLING_ENABLED on the gateway."
      : "No spend recorded yet.";
  const prepaid = status?.billing_enabled ?? true;

  return (
    <>
      <BillingPageHeader />
      {status && <BillingToolbar status={status} scope={scope} />}
      <SectionsGroup>
        <SectionLayout>
          <SectionHeader heading={prepaid ? "Wallet" : "Spend"} />
          <div className="grid grid-cols-2 gap-3 md:grid-cols-3">
            {walletBalance?.available_micros != null && (
              <StatCard
                label="Mint available balance"
                value={formatUsd(walletBalance.available_micros)}
                hint={`Payer account for ${walletBalance.principal}`}
              />
            )}
            <StatCard
              label="Total charged"
              value={formatUsd(wallet.total_charged_micros)}
              hint={
                prepaid
                  ? undefined
                  : "Postpaid: invoiced by the cost-feed consumer"
              }
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
                <EmptyRow colSpan={3} message={emptyMessage} />
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
                <EmptyRow colSpan={4} message={emptyMessage} />
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
                <EmptyRow
                  colSpan={4}
                  message={
                    prepaid
                      ? "No settlements recorded yet."
                      : "Postpaid metering does not create Mint settlements."
                  }
                />
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
  const { page, status, scope } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<BillingSkeleton />}>
        <Await
          resolve={page}
          errorElement={<BillingErrorState status={status} />}
        >
          {([resolvedSummary, resolvedStatus, resolvedWallet]) => (
            <BillingContent
              summary={resolvedSummary}
              status={resolvedStatus}
              scope={scope}
              wallet={resolvedWallet}
            />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
