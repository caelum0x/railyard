import type { Route } from "./+types/route";
import { Suspense, useMemo } from "react";
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
  KeySpendPoint,
  KeySpendSeries,
} from "~/types/billing";
import { ScopeToggle, scopeFromUrl } from "~/components/billing/BillingShared";

const MICROS_PER_USD = 1_000_000;

export const handle: RouteHandle = {
  crumb: () => ["Billing", "API Keys"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope = scopeFromUrl(request.url);
  const client = getTensorZeroClient();

  // Deferred: the gateway buckets the durable cost-event table into per-key,
  // per-day spend points and streams them in via `Await`.
  const page = Promise.all([
    client.getBillingKeySpend({ scope }),
    client.getBillingStatus().catch((): BillingStatus | null => null),
  ]);

  return { page, scope };
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

function formatDay(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

interface KeyRollup {
  key: string;
  charged_micros: number;
  total_tokens: number;
  cost_event_count: number;
}

function rollupByKey(points: KeySpendPoint[]): KeyRollup[] {
  const byKey = new Map<string, KeyRollup>();
  for (const point of points) {
    const existing = byKey.get(point.key) ?? {
      key: point.key,
      charged_micros: 0,
      total_tokens: 0,
      cost_event_count: 0,
    };
    byKey.set(point.key, {
      key: point.key,
      charged_micros: existing.charged_micros + point.charged_micros,
      total_tokens: existing.total_tokens + point.total_tokens,
      cost_event_count: existing.cost_event_count + point.cost_event_count,
    });
  }
  return [...byKey.values()].sort(
    (left, right) => right.charged_micros - left.charged_micros,
  );
}

function KeysPageHeader() {
  return <PageHeader heading="API Key Spend" />;
}

function KeysSkeleton() {
  return (
    <>
      <KeysPageHeader />
      <SectionLayout>
        <Skeleton className="h-32 w-full rounded-md" />
        <Skeleton className="h-64 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function KeysErrorState() {
  const error = useAsyncError();
  return (
    <>
      <KeysPageHeader />
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

function KeysContent({
  series,
  status,
  scope,
}: {
  series: KeySpendSeries;
  status: BillingStatus | null;
  scope: BillingScope;
}) {
  const { points } = series;
  const totals = useMemo(() => rollupByKey(points), [points]);
  const sortedPoints = useMemo(
    () => [...points].sort((left, right) => right.bucket_start_ms - left.bucket_start_ms),
    [points],
  );

  return (
    <>
      <KeysPageHeader />
      <div className="flex flex-wrap items-center justify-between gap-3">
        <ScopeToggle scope={scope} operator={status?.operator ?? false} />
        <Link
          to={scope === "all" ? "/billing/budgets?scope=all" : "/billing/budgets"}
          className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
        >
          Manage monthly spend caps
        </Link>
      </div>
      <SectionsGroup>
        <SectionLayout>
          <SectionHeader heading="Spend by key" />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>API key</TableHead>
                <TableHead className="text-right">Cost events</TableHead>
                <TableHead className="text-right">Tokens</TableHead>
                <TableHead className="text-right">Charged</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {totals.length === 0 ? (
                <EmptyRow colSpan={4} message="No spend recorded yet." />
              ) : (
                totals.map((row) => (
                  <TableRow key={row.key}>
                    <TableCell className="font-mono">{row.key}</TableCell>
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
          <SectionHeader heading="Daily spend" />
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
              {sortedPoints.length === 0 ? (
                <EmptyRow colSpan={5} message="No spend recorded yet." />
              ) : (
                sortedPoints.map((point) => (
                  <TableRow key={`${point.key}-${point.bucket_start_ms}`}>
                    <TableCell>{formatDay(point.bucket_start_ms)}</TableCell>
                    <TableCell className="font-mono">{point.key}</TableCell>
                    <TableCell className="text-right">
                      {formatCount(point.cost_event_count)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatCount(point.total_tokens)}
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
    </>
  );
}

export default function BillingKeysPage({ loaderData }: Route.ComponentProps) {
  const { page, scope } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<KeysSkeleton />}>
        <Await resolve={page} errorElement={<KeysErrorState />}>
          {([series, status]) => (
            <KeysContent series={series} status={status} scope={scope} />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
