import type { Route } from "./+types/route";
import { Suspense, useState } from "react";
import {
  Await,
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
const SETTLEMENT_HISTORY_LIMIT = 200;

export const handle: RouteHandle = {
  crumb: () => ["Billing", "Settlements"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope = scopeFromUrl(request.url);

  // Deferred: the settlement-evidence detail rows are aggregated by the gateway
  // from the durable `relay_settlement_evidence` table and streamed via `Await`.
  const history = getTensorZeroClient().getBillingSettlements({
    settlements: SETTLEMENT_HISTORY_LIMIT,
    scope,
  });

  return { history };
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

function SettlementsContent({ history }: { history: SettlementHistory }) {
  const { settlements } = history;

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
                <TableCell
                  colSpan={6}
                  className="text-fg-muted text-center"
                >
                  No settlements recorded yet.
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
      </SectionLayout>
    </>
  );
}

export default function BillingSettlementsPage({
  loaderData,
}: Route.ComponentProps) {
  const { history } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<SettlementsSkeleton />}>
        <Await resolve={history} errorElement={<SettlementsErrorState />}>
          {(resolved) => <SettlementsContent history={resolved} />}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
