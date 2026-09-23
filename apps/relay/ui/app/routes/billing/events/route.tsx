import type { Route } from "./+types/route";
import { Suspense } from "react";
import {
  Await,
  Form,
  Link,
  data,
  useActionData,
  useAsyncError,
  useLocation,
  useNavigation,
  type RouteHandle,
} from "react-router";
import { RotateCcw } from "lucide-react";
import {
  PageHeader,
  PageLayout,
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
import { Button } from "~/components/ui/button";
import { Badge } from "~/components/ui/badge";
import { Skeleton } from "~/components/ui/skeleton";
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { logger } from "~/utils/logger";
import {
  BillingStatusBanner,
  ScopeToggle,
  formatCount,
  formatTimestamp,
  formatUsd,
  scopeFromUrl,
} from "~/components/billing/BillingShared";
import {
  costEventState,
  isMeteredEvent,
  type BillingScope,
  type BillingStatus,
  type CostEventPage,
  type CostEventRecord,
  type CostEventState,
} from "~/types/billing";

const PAGE_SIZE = 50;
const STATES: { value: CostEventState | null; label: string }[] = [
  { value: null, label: "All" },
  { value: "pending", label: "Pending" },
  { value: "published", label: "Delivered" },
  { value: "dead_lettered", label: "Dead-lettered" },
];

export const handle: RouteHandle = {
  crumb: () => ["Billing", "Cost Events"],
};

function parseState(raw: string | null): CostEventState | undefined {
  return raw === "pending" || raw === "published" || raw === "dead_lettered"
    ? raw
    : undefined;
}

function parseCursor(raw: string | null): string | undefined {
  // Event ids are UUIDs; reject anything else before it reaches the gateway.
  return raw && /^[0-9a-f-]{36}$/i.test(raw) ? raw : undefined;
}

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const url = new URL(request.url);
  const scope = scopeFromUrl(url);
  const state = parseState(url.searchParams.get("state"));
  const before = parseCursor(url.searchParams.get("before"));
  const client = getTensorZeroClient();

  const page = Promise.all([
    client.listCostEvents({ state, before, limit: PAGE_SIZE, scope }),
    client.getBillingStatus().catch((): BillingStatus | null => null),
  ]);

  return { page, scope, state: state ?? null, before: before ?? null };
}

export async function action({ request }: Route.ActionArgs) {
  await requireValidApiKeyIfEnabled();
  const formData = await request.formData();
  const eventId = formData.get("eventId");
  const scope: BillingScope = formData.get("scope") === "all" ? "all" : "own";
  if (typeof eventId !== "string" || !parseCursor(eventId)) {
    return data({ error: "A valid cost event id is required." }, { status: 400 });
  }
  try {
    await getTensorZeroClient().replayCostEvent(eventId, { scope });
    return { replayed: eventId };
  } catch (error) {
    logger.error("Failed to replay Relay cost event", error);
    const message =
      error instanceof Error ? error.message : "Replay failed. Try again.";
    return data({ error: message }, { status: 502 });
  }
}

function EventsPageHeader() {
  return <PageHeader heading="Cost Events" />;
}

function EventsSkeleton() {
  return (
    <>
      <EventsPageHeader />
      <SectionLayout>
        <Skeleton className="h-16 w-full rounded-md" />
        <Skeleton className="h-64 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function EventsErrorState() {
  const error = useAsyncError();
  return (
    <>
      <EventsPageHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

function StateBadge({ record }: { record: CostEventRecord }) {
  const state = costEventState(record);
  if (state === "published") {
    return <Badge variant="outline">Delivered</Badge>;
  }
  if (state === "dead_lettered") {
    return <Badge variant="destructive">Dead-lettered</Badge>;
  }
  return <Badge variant="secondary">Pending</Badge>;
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

function EventsContent({
  events,
  status,
  scope,
  state,
  before,
}: {
  events: CostEventPage;
  status: BillingStatus | null;
  scope: BillingScope;
  state: CostEventState | null;
  before: string | null;
}) {
  const location = useLocation();
  const navigation = useNavigation();
  const actionData = useActionData<typeof action>();
  const replaying = navigation.state !== "idle" ? navigation.formData?.get("eventId") : null;
  const showKey = scope === "all";
  const colSpan = showKey ? 9 : 8;

  return (
    <>
      <EventsPageHeader />
      <SectionLayout>
        {status && <BillingStatusBanner status={status} />}
        <p className="text-fg-secondary text-sm">
          Every captured or metered inference is written to a durable outbox and
          delivered at least once to the cost feed (for example Rago). Failed
          deliveries retry with backoff and are dead-lettered after repeated
          failures; replay them here once the cause is fixed.
        </p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="bg-bg-tertiary inline-flex w-fit items-center gap-1 rounded-md p-1">
            {STATES.map((option) => (
              <Link
                key={option.label}
                to={hrefWith(location.pathname, location.search, {
                  state: option.value,
                  before: null,
                })}
                preventScrollReset
                className={
                  option.value === state
                    ? "bg-bg-primary text-fg-primary rounded px-3 py-1 text-sm font-medium shadow-sm"
                    : "text-fg-secondary hover:text-fg-primary rounded px-3 py-1 text-sm"
                }
              >
                {option.label}
              </Link>
            ))}
          </div>
          <ScopeToggle scope={scope} operator={status?.operator ?? false} />
        </div>
        {actionData && "error" in actionData && (
          <p className="text-sm text-red-600" role="alert">
            {actionData.error}
          </p>
        )}
        {actionData && "replayed" in actionData && (
          <p className="text-sm text-green-700" role="status">
            Event {actionData.replayed} was re-queued for delivery.
          </p>
        )}
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Event</TableHead>
              {showKey && <TableHead>API key</TableHead>}
              <TableHead>Model</TableHead>
              <TableHead className="text-right">Tokens</TableHead>
              <TableHead className="text-right">Charged</TableHead>
              <TableHead>Mode</TableHead>
              <TableHead>Delivery</TableHead>
              <TableHead>Recorded</TableHead>
              <TableHead className="w-0"></TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {events.items.length === 0 ? (
              <TableRow>
                <TableCell colSpan={colSpan} className="text-fg-muted text-center">
                  {state
                    ? "No cost events in this state."
                    : "No cost events recorded yet."}
                </TableCell>
              </TableRow>
            ) : (
              events.items.map((record) => {
                const { event } = record;
                const deadLettered = costEventState(record) === "dead_lettered";
                return (
                  <TableRow key={event.event_id}>
                    <TableCell className="font-mono text-xs">
                      {event.event_id}
                    </TableCell>
                    {showKey && (
                      <TableCell className="font-mono">
                        {event.owner_principal}
                      </TableCell>
                    )}
                    <TableCell className="font-mono text-xs">{event.model}</TableCell>
                    <TableCell className="text-right">
                      {formatCount(event.usage.total_tokens)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(event.charged_micros)}
                    </TableCell>
                    <TableCell>
                      {isMeteredEvent(event) ? "Metered" : "Mint settled"}
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col gap-1">
                        <StateBadge record={record} />
                        <span className="text-fg-muted text-xs">
                          {formatCount(record.attempts)} attempt
                          {record.attempts === 1 ? "" : "s"}
                        </span>
                        {record.last_error && (
                          <span
                            className="max-w-xs truncate text-xs text-red-600"
                            title={record.last_error}
                          >
                            {record.last_error}
                          </span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-xs">
                      {formatTimestamp(event.settled_at_ms)}
                    </TableCell>
                    <TableCell>
                      {deadLettered && (
                        <Form method="post" preventScrollReset>
                          <input type="hidden" name="eventId" value={event.event_id} />
                          <input type="hidden" name="scope" value={scope} />
                          <Button
                            type="submit"
                            variant="outline"
                            size="sm"
                            disabled={replaying === event.event_id}
                          >
                            <RotateCcw aria-hidden />
                            {replaying === event.event_id ? "Replaying" : "Replay"}
                          </Button>
                        </Form>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })
            )}
          </TableBody>
        </Table>
        <div className="flex items-center justify-center gap-3">
          {before && (
            <Link
              to={hrefWith(location.pathname, location.search, { before: null })}
              className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
            >
              Newest events
            </Link>
          )}
          {events.next_cursor && (
            <Link
              to={hrefWith(location.pathname, location.search, {
                before: events.next_cursor,
              })}
              className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
            >
              Older events
            </Link>
          )}
        </div>
      </SectionLayout>
    </>
  );
}

export default function CostEventsPage({ loaderData }: Route.ComponentProps) {
  const { page, scope, state, before } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<EventsSkeleton />}>
        <Await resolve={page} errorElement={<EventsErrorState />}>
          {([events, status]) => (
            <EventsContent
              events={events}
              status={status}
              scope={scope}
              state={state}
              before={before}
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
