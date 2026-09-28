import type { Route } from "./+types/route";
import { Suspense, useEffect } from "react";
import {
  Await,
  Form,
  Link,
  useAsyncError,
  useLocation,
  useRevalidator,
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
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { BillingStatus } from "~/types/billing";
import type { TracePage } from "~/types/traces";
import { InlineNotice, ScopeToggle } from "~/components/billing/BillingShared";
import {
  REFRESH_INTERVAL_MS,
  WINDOW_OPTIONS,
  filtersFromUrl,
  formatLatency,
  formatTokens,
  formatTraceCost,
  formatTraceTime,
  hrefForFilters,
  traceStatus,
  type TraceFilters,
} from "./format";

const PAGE_SIZE = 50;

export const handle: RouteHandle = {
  crumb: () => ["Key Traces"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const filters = filtersFromUrl(request.url);
  const client = getTensorZeroClient();
  const page = Promise.all([
    client.listRelayTraces({
      scope: filters.scope,
      key: filters.key,
      limit: PAGE_SIZE,
      windowHours: filters.windowHours,
      before: filters.before,
      functionName: filters.functionName,
    }),
    client.getBillingStatus().catch((): BillingStatus | null => null),
  ]);
  return { page, filters };
}

function TracesHeader() {
  return <PageHeader heading="Traces by API key" />;
}

function TracesSkeleton() {
  return (
    <>
      <TracesHeader />
      <SectionLayout>
        <Skeleton className="h-10 w-full rounded-md" />
        <Skeleton className="h-64 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function TracesError() {
  const error = useAsyncError();
  return (
    <>
      <TracesHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

/** Refresh the first page so a new call shows up without a reload. */
function useLiveRefresh(enabled: boolean) {
  const revalidator = useRevalidator();
  useEffect(() => {
    if (!enabled) {
      return undefined;
    }
    const timer = window.setInterval(() => {
      if (
        revalidator.state === "idle" &&
        document.visibilityState === "visible"
      ) {
        void revalidator.revalidate();
      }
    }, REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [enabled, revalidator]);
}

function FilterBar({
  filters,
  operator,
}: {
  filters: TraceFilters;
  operator: boolean;
}) {
  return (
    <Form method="get" className="flex flex-wrap items-end gap-3 text-sm">
      {filters.scope === "all" && (
        <input type="hidden" name="scope" value="all" />
      )}
      <label className="flex flex-col gap-1">
        <span className="text-fg-secondary">Window</span>
        <select
          name="window_hours"
          defaultValue={String(filters.windowHours)}
          className="border-border bg-bg-primary rounded-md border px-2 py-1"
        >
          {WINDOW_OPTIONS.map((option) => (
            <option key={option.hours} value={option.hours}>
              {option.label}
            </option>
          ))}
        </select>
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-fg-secondary">Function</span>
        <input
          name="function"
          defaultValue={filters.functionName ?? ""}
          placeholder="any"
          className="border-border bg-bg-primary rounded-md border px-2 py-1 font-mono"
        />
      </label>
      {operator && filters.scope === "all" && (
        <label className="flex flex-col gap-1">
          <span className="text-fg-secondary">API key</span>
          <input
            name="key"
            defaultValue={filters.key ?? ""}
            placeholder="all keys"
            className="border-border bg-bg-primary rounded-md border px-2 py-1 font-mono"
          />
        </label>
      )}
      <button
        type="submit"
        className="bg-bg-tertiary hover:bg-bg-secondary rounded-md px-3 py-1 font-medium"
      >
        Apply
      </button>
    </Form>
  );
}

function TraceRows({
  page,
  filters,
}: {
  page: TracePage;
  filters: TraceFilters;
}) {
  const showKey = filters.scope === "all";
  const columns = showKey ? 9 : 8;
  if (page.traces.length === 0) {
    return (
      <TableRow>
        <TableCell colSpan={columns} className="text-fg-muted text-center">
          No traces in this window yet. Calls made with this key appear here
          within seconds.
        </TableCell>
      </TableRow>
    );
  }
  const detailQuery = filters.scope === "all" ? "?scope=all" : "";
  return (
    <>
      {page.traces.map((trace) => (
        <TableRow key={trace.inference_id}>
          <TableCell className="font-mono text-xs">
            <Link
              to={`/observability/keys/${trace.inference_id}${detailQuery}`}
              className="underline-offset-4 hover:underline"
            >
              {formatTraceTime(trace.created_at_us)}
            </Link>
          </TableCell>
          {showKey && (
            <TableCell className="font-mono text-xs">
              {trace.api_key_public_id ?? "unauthenticated"}
            </TableCell>
          )}
          <TableCell className="font-mono text-xs">
            {trace.function_name}
          </TableCell>
          <TableCell className="font-mono text-xs">
            {trace.model_name ?? "-"}
            {trace.model_provider_name ? ` / ${trace.model_provider_name}` : ""}
          </TableCell>
          <TableCell className="text-right">
            {formatTokens(trace.input_tokens)}
          </TableCell>
          <TableCell className="text-right">
            {formatTokens(trace.output_tokens)}
          </TableCell>
          <TableCell className="text-right">
            {formatLatency(trace.processing_time_ms)}
          </TableCell>
          <TableCell className="text-right">
            {formatTraceCost(trace.cost_micros)}
          </TableCell>
          <TableCell>
            {traceStatus(trace)}
            {trace.model_calls > 1 ? ` (${trace.model_calls} calls)` : ""}
          </TableCell>
        </TableRow>
      ))}
    </>
  );
}

function TracesContent({
  page,
  status,
  filters,
}: {
  page: TracePage;
  status: BillingStatus | null;
  filters: TraceFilters;
}) {
  const location = useLocation();
  const operator = status?.operator ?? false;
  useLiveRefresh(!filters.before);
  return (
    <>
      <TracesHeader />
      <div className="flex flex-wrap items-center justify-between gap-3">
        <ScopeToggle scope={filters.scope} operator={operator} />
        <FilterBar filters={filters} operator={operator} />
      </div>
      <InlineNotice>
        {filters.scope === "all"
          ? page.key
            ? `Showing calls made with key ${page.key}.`
            : "Showing calls from every API key."
          : `Showing calls made with ${status?.principal ? `key ${status.principal}` : "this API key"}. Other keys never see these traces.`}{" "}
        Prompts and outputs are kept 14 days; the rows below are kept 90 days.
      </InlineNotice>
      <SectionLayout>
        <SectionHeader heading="Recent calls" />
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Time (UTC)</TableHead>
              {filters.scope === "all" && <TableHead>API key</TableHead>}
              <TableHead>Function</TableHead>
              <TableHead>Model / provider</TableHead>
              <TableHead className="text-right">Input tokens</TableHead>
              <TableHead className="text-right">Output tokens</TableHead>
              <TableHead className="text-right">Latency</TableHead>
              <TableHead className="text-right">Cost</TableHead>
              <TableHead>Status</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TraceRows page={page} filters={filters} />
          </TableBody>
        </Table>
        <div className="flex justify-end gap-4 text-sm">
          {filters.before && (
            <Link
              to={hrefForFilters(location.pathname, {
                ...filters,
                before: undefined,
              })}
              className="text-fg-secondary hover:text-fg-primary underline-offset-4 hover:underline"
            >
              Newest
            </Link>
          )}
          {page.next_before && (
            <Link
              to={hrefForFilters(location.pathname, {
                ...filters,
                before: page.next_before,
              })}
              className="text-fg-secondary hover:text-fg-primary underline-offset-4 hover:underline"
            >
              Older
            </Link>
          )}
        </div>
      </SectionLayout>
    </>
  );
}

export default function KeyTracesPage({ loaderData }: Route.ComponentProps) {
  const { page, filters } = loaderData;
  return (
    <PageLayout>
      <Suspense fallback={<TracesSkeleton />}>
        <Await resolve={page} errorElement={<TracesError />}>
          {([tracePage, status]) => (
            <TracesContent page={tracePage} status={status} filters={filters} />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
