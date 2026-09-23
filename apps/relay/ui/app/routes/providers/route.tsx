import type { Route } from "./+types/route";
import { Suspense } from "react";
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
import { Badge } from "~/components/ui/badge";
import { Skeleton } from "~/components/ui/skeleton";
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { formatCount, formatTimestamp } from "~/components/billing/BillingShared";
import type {
  ModelRoutingStatus,
  ProviderHealthReport,
  ProviderRouteStatus,
} from "~/types/catalog";

export const handle: RouteHandle = {
  crumb: () => ["Provider Health"],
};

export async function loader() {
  await requireValidApiKeyIfEnabled();
  const report = getTensorZeroClient().getProviderHealth();
  return { report };
}

function ProvidersPageHeader() {
  return <PageHeader heading="Provider Health" />;
}

function ProvidersSkeleton() {
  return (
    <>
      <ProvidersPageHeader />
      <SectionLayout>
        <Skeleton className="h-48 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function ProvidersErrorState() {
  const error = useAsyncError();
  return (
    <>
      <ProvidersPageHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

function formatMs(value: number, observed: boolean): string {
  if (!observed) {
    return "—";
  }
  return `${Math.round(value).toLocaleString()} ms`;
}

function formatRate(value: number, observed: boolean): string {
  if (!observed) {
    return "—";
  }
  return `${(value * 100).toFixed(1)}%`;
}

function CircuitBadge({ route }: { route: ProviderRouteStatus }) {
  if (route.circuit === "open") {
    const seconds = Math.ceil((route.circuit_cooldown_ms ?? 0) / 1000);
    return (
      <Badge variant="destructive">
        Open{seconds > 0 ? ` (${seconds}s)` : ""}
      </Badge>
    );
  }
  if (route.circuit === "half_open") {
    return <Badge variant="warning">Probing</Badge>;
  }
  if (!route.observed) {
    return <Badge variant="outline">No traffic</Badge>;
  }
  return <Badge variant="secondary">Healthy</Badge>;
}

function RouteRows({ routes }: { routes: ProviderRouteStatus[] }) {
  return (
    <>
      {routes.map((route) => (
        <TableRow key={`${route.priority}-${route.provider}`}>
          <TableCell className="font-mono">{route.provider}</TableCell>
          <TableCell className="text-right">
            {route.priority === 0 ? "Primary" : `Fallback ${route.priority}`}
          </TableCell>
          <TableCell>
            <CircuitBadge route={route} />
          </TableCell>
          <TableCell className="text-right">
            {formatMs(route.ewma_latency_ms, route.observed)}
          </TableCell>
          <TableCell className="text-right">
            {formatMs(route.ewma_ttft_ms, route.observed)}
          </TableCell>
          <TableCell className="text-right">
            {formatRate(route.error_rate, route.observed)}
          </TableCell>
          <TableCell className="text-right">
            {formatCount(route.total_attempts)} /{" "}
            {formatCount(route.failed_attempts)}
          </TableCell>
        </TableRow>
      ))}
    </>
  );
}

function RouteTable({ routes }: { routes: ProviderRouteStatus[] }) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Provider</TableHead>
          <TableHead className="text-right">Routing</TableHead>
          <TableHead>Circuit</TableHead>
          <TableHead className="text-right">Latency (EWMA)</TableHead>
          <TableHead className="text-right">Time to first token</TableHead>
          <TableHead className="text-right">Error rate</TableHead>
          <TableHead className="text-right">Attempts / failures</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <RouteRows routes={routes} />
      </TableBody>
    </Table>
  );
}

function ModelSection({ model }: { model: ModelRoutingStatus }) {
  return (
    <SectionLayout>
      <div className="flex flex-wrap items-center gap-2">
        <SectionHeader heading={model.model} />
        {model.has_fallback ? (
          <Badge variant="secondary">
            {model.providers.length} providers
          </Badge>
        ) : (
          <Badge variant="warning">Single provider, no model-level failover</Badge>
        )}
        {model.hedging_enabled && (
          <Badge variant="outline">
            Hedged x{model.hedge_max_concurrency}
          </Badge>
        )}
      </div>
      <RouteTable routes={model.providers} />
    </SectionLayout>
  );
}

function ProvidersContent({ report }: { report: ProviderHealthReport }) {
  const openCircuits = report.models
    .flatMap((model) => model.providers)
    .filter((route) => route.circuit === "open").length;
  return (
    <>
      <ProvidersPageHeader />
      <SectionsGroup>
        <SectionLayout>
          <p className="text-fg-secondary text-sm">
            Relay ranks each model&apos;s providers by recent latency and error
            rate, and opens a circuit breaker on a provider that keeps failing so
            traffic moves to the next route. Function-level failover (for
            example the Relay Chat fallback variant) is configured in the gateway
            config. Snapshot taken {formatTimestamp(report.generated_at_ms)}
            {openCircuits > 0 ? `; ${openCircuits} circuit(s) open.` : "."}
          </p>
        </SectionLayout>
        {report.models.length === 0 ? (
          <SectionLayout>
            <p className="text-fg-muted text-sm">No models are configured.</p>
          </SectionLayout>
        ) : (
          report.models.map((model) => (
            <ModelSection key={model.model} model={model} />
          ))
        )}
        {report.unrouted.length > 0 && (
          <SectionLayout>
            <SectionHeader heading="Retired routes" />
            <RouteTable routes={report.unrouted} />
          </SectionLayout>
        )}
      </SectionsGroup>
    </>
  );
}

export default function ProvidersPage({ loaderData }: Route.ComponentProps) {
  const { report } = loaderData;
  const location = useLocation();
  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<ProvidersSkeleton />}>
        <Await resolve={report} errorElement={<ProvidersErrorState />}>
          {(resolved) => <ProvidersContent report={resolved} />}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
