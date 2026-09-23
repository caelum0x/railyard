import type { Route } from "./+types/route";
import { Suspense, useState } from "react";
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
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { SettlementDetail, SettlementHistory } from "~/types/billing";
import { scopeFromUrl } from "~/components/billing/BillingShared";

const MICROS_PER_USD = 1_000_000;
const SETTLEMENT_PAGE_SIZE = 100;
/** `<settled_at_ms>:<request_id>` as issued by the gateway's `next_before`. */
const CURSOR_PATTERN = /^\d{1,16}:[0-9a-fA-F-]{36}$/;

export const handle: RouteHandle = {
  crumb: () => ["Billing", "Settlements"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const url = new URL(request.url);
  const scope = scopeFromUrl(url);
  const rawBefore = url.searchParams.get("before");
  const before = rawBefore && CURSOR_PATTERN.test(rawBefore) ? rawBefore : null;

  // Deferred: the settlement-evidence detail rows are aggregated by the gateway
  // from the durable `relay_settlement_evidence` table and streamed via `Await`.
  // Pages are keyset-paginated with the gateway's `next_before` cursor.
  const history = getTensorZeroClient().getBillingSettlements({
    settlements: SETTLEMENT_PAGE_SIZE,
    scope,
    before: before ?? undefined,
  });

  return { history, before };
}

function hrefWith(
  pathname: string,
  search: string,
  changes: Record<string, string | null>,
): string {
  const params = new URLSearchParams(search);
  for (const [key, value] of Object.entries(changes)) {
    if (value == null) {
      params.delete(key);
    } else {
      params.set(key, value);
    }
  }
  const query = params.toString();
  return `${pathname}${query ? `?${query}` : ""}`;
}

function formatUsd(micros: number): string {
  return (micros / MICROS_PER_USD).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

function formatTimestamp(ms: number | undefined): string {
  if (ms == null) {
    return "—";
  }
  return new Date(ms).toLocaleString();
}

function prettyJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

function SettlementsPageHeader() {
  return <PageHeader heading="Settlements" />;
}

function SettlementsSkeleton() {
  return (
    <>
      <SettlementsPageHeader />
      <SectionLayout>
        <Skeleton className="h-64 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function SettlementsErrorState() {
  const error = useAsyncError();
  return (
    <>
      <SettlementsPageHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

function SettlementRow({ settlement }: { settlement: SettlementDetail }) {
  const [open, setOpen] = useState(false);

  return (
    <>
      <TableRow>
        <TableCell className="font-mono">{settlement.request_id}</TableCell>
        <TableCell className="text-right">
          {formatUsd(settlement.amount_micros)}
        </TableCell>
        <TableCell className="text-right">
          {formatTimestamp(settlement.settled_at_ms)}
        </TableCell>
        <TableCell className="text-right">
          {formatTimestamp(settlement.verified_at_ms)}
        </TableCell>
        <TableCell className="text-center">
          {settlement.has_attestation ? (
            <Badge variant="secondary">Attested</Badge>
          ) : (
            <span className="text-fg-muted">—</span>
          )}
        </TableCell>
        <TableCell className="text-right">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setOpen((value) => !value)}
            aria-expanded={open}
          >
            {open ? "Hide receipt" : "View receipt"}
          </Button>
        </TableCell>
      </TableRow>
      {open && (
        <TableRow>
          <TableCell colSpan={6} className="bg-bg-secondary">
            <pre className="text-fg-secondary max-h-96 overflow-auto rounded-md p-3 font-mono text-xs whitespace-pre">
              {prettyJson(settlement.receipt_json)}
            </pre>
          </TableCell>
        </TableRow>
      )}
    </>
  );
}

function SettlementsContent({
  history,
  before,
}: {
  history: SettlementHistory;
  before: string | null;
}) {
  const { settlements } = history;
  const location = useLocation();

  return (
    <>
      <SettlementsPageHeader />
      <SectionLayout>
        <SectionHeader heading="Settlement evidence" />
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Request ID</TableHead>
              <TableHead className="text-right">Amount</TableHead>
              <TableHead className="text-right">Settled</TableHead>
              <TableHead className="text-right">Verified</TableHead>
              <TableHead className="text-center">Attestation</TableHead>
              <TableHead className="text-right">Receipt</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {settlements.length === 0 ? (
              <TableRow>
                <TableCell colSpan={6} className="text-fg-muted text-center">
                  {before
                    ? "No older settlements."
                    : "No settlements recorded yet."}
                </TableCell>
              </TableRow>
            ) : (
              settlements.map((settlement) => (
                <SettlementRow
                  key={settlement.request_id}
                  settlement={settlement}
                />
              ))
            )}
          </TableBody>
        </Table>
        <div className="flex items-center justify-center gap-3">
          {before && (
            <Link
              to={hrefWith(location.pathname, location.search, {
                before: null,
              })}
              className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
            >
              Newest settlements
            </Link>
          )}
          {history.next_before && (
            <Link
              to={hrefWith(location.pathname, location.search, {
                before: history.next_before,
              })}
              className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
            >
              Older settlements
            </Link>
          )}
        </div>
      </SectionLayout>
    </>
  );
}

export default function BillingSettlementsPage({
  loaderData,
}: Route.ComponentProps) {
  const { history, before } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<SettlementsSkeleton />}>
        <Await resolve={history} errorElement={<SettlementsErrorState />}>
          {(resolved) => (
            <SettlementsContent history={resolved} before={before} />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
