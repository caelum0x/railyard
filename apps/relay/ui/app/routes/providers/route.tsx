import type { Route } from "./+types/route";
import { Suspense } from "react";
import {
  Await,
  Form,
  data,
  useActionData,
  useAsyncError,
  useLocation,
  useNavigation,
  type RouteHandle,
} from "react-router";
import { KeyRound, PlugZap, Trash2 } from "lucide-react";
import { Button } from "~/components/ui/button";
import { Input } from "~/components/ui/input";
import { Label } from "~/components/ui/label";
import { logger } from "~/utils/logger";
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
import {
  InlineNotice,
  formatCount,
  formatTimestamp,
} from "~/components/billing/BillingShared";
import type {
  ModelRoutingStatus,
  ProviderHealthReport,
  ProviderKey,
  ProviderKeysState,
  ProviderRouteStatus,
} from "~/types/catalog";

export const handle: RouteHandle = {
  crumb: () => ["Providers"],
};

const CREDENTIAL_NAME_PATTERN = /^relay_[a-z0-9_]{1,64}$/;

export async function loader() {
  await requireValidApiKeyIfEnabled();
  const client = getTensorZeroClient();
  const report = client.getProviderHealth();
  const keys = client
    .listProviderKeys()
    .catch((error: unknown): ProviderKeysState | null => {
      logger.error("Relay provider keys unavailable", error);
      return null;
    });
  // One deferred page: provider health plus the credential slots.
  const page = Promise.all([report, keys]);
  return { page };
}

type KeyActionResult =
  | { ok: true; name: string; message: string }
  | { ok: false; name: string; error: string };

function actionErrorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

export async function action({ request }: Route.ActionArgs) {
  await requireValidApiKeyIfEnabled();
  const formData = await request.formData();
  const intent = String(formData.get("intent") ?? "");
  const name = String(formData.get("name") ?? "");
  if (!CREDENTIAL_NAME_PATTERN.test(name)) {
    return data<KeyActionResult>(
      { ok: false, name, error: "Unknown provider credential." },
      { status: 400 },
    );
  }
  const client = getTensorZeroClient();
  try {
    if (intent === "set_key") {
      const apiKey = String(formData.get("api_key") ?? "").trim();
      if (apiKey.length < 8 || /\s/.test(apiKey)) {
        return data<KeyActionResult>(
          {
            ok: false,
            name,
            error: "Paste the full API key (at least 8 characters, no spaces).",
          },
          { status: 400 },
        );
      }
      const saved = await client.setProviderKey(name, apiKey);
      return data<KeyActionResult>({
        ok: true,
        name,
        message: `Saved ${saved.label} key ${saved.key_hint ?? ""}. New requests use it right away; run a connection test to confirm it works.`,
      });
    }
    if (intent === "delete_key") {
      const removed = await client.deleteProviderKey(name);
      return data<KeyActionResult>({
        ok: true,
        name,
        message:
          removed.source === "environment"
            ? `Removed the stored ${removed.label} key; requests now use ${removed.env_var}.`
            : `Removed the stored ${removed.label} key. No key is configured for this provider now.`,
      });
    }
    if (intent === "test_key") {
      const result = await client.testProviderKey(name);
      return data<KeyActionResult>(
        result.ok
          ? { ok: true, name, message: result.message }
          : { ok: false, name, error: result.message },
      );
    }
  } catch (error) {
    logger.error("Relay provider key action failed", error);
    return data<KeyActionResult>(
      {
        ok: false,
        name,
        error: actionErrorMessage(error, "The gateway rejected the request."),
      },
      { status: 502 },
    );
  }
  return data<KeyActionResult>(
    { ok: false, name, error: "Unknown action." },
    { status: 400 },
  );
}

function ProvidersPageHeader() {
  return <PageHeader heading="Providers" />;
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
          <Badge variant="secondary">{model.providers.length} providers</Badge>
        ) : (
          <Badge variant="warning">
            Single provider, no model-level failover
          </Badge>
        )}
        {model.hedging_enabled && (
          <Badge variant="outline">Hedged x{model.hedge_max_concurrency}</Badge>
        )}
      </div>
      <RouteTable routes={model.providers} />
    </SectionLayout>
  );
}

function KeySourceBadge({ providerKey }: { providerKey: ProviderKey }) {
  if (providerKey.error) {
    return <Badge variant="destructive">Unusable</Badge>;
  }
  if (providerKey.source === "stored") {
    return <Badge variant="secondary">Stored in Relay</Badge>;
  }
  if (providerKey.source === "environment") {
    return <Badge variant="outline">From {providerKey.env_var}</Badge>;
  }
  return <Badge variant="warning">Not configured (disabled)</Badge>;
}

function ProviderKeyRow({
  providerKey,
  canManage,
}: {
  providerKey: ProviderKey;
  canManage: boolean;
}) {
  const navigation = useNavigation();
  const result = useActionData<typeof action>() as KeyActionResult | undefined;
  const busy =
    navigation.state !== "idle" &&
    navigation.formData?.get("name") === providerKey.name;
  const rowResult = result?.name === providerKey.name ? result : undefined;
  return (
    <TableRow>
      <TableCell>
        <div className="font-medium">{providerKey.label}</div>
        <div className="text-fg-muted font-mono text-xs">
          dynamic::{providerKey.name}
        </div>
      </TableCell>
      <TableCell>
        <div className="flex flex-col gap-1">
          <KeySourceBadge providerKey={providerKey} />
          {providerKey.key_hint && (
            <span className="text-fg-muted font-mono text-xs">
              {providerKey.key_hint}
            </span>
          )}
          {providerKey.error && (
            <span className="text-xs text-red-600">{providerKey.error}</span>
          )}
        </div>
      </TableCell>
      <TableCell className="text-xs">
        {providerKey.updated_at_ms != null ? (
          <div className="flex flex-col">
            <span>{formatTimestamp(providerKey.updated_at_ms)}</span>
            <span className="text-fg-muted font-mono">
              {providerKey.updated_by}
            </span>
          </div>
        ) : (
          <span className="text-fg-muted">—</span>
        )}
      </TableCell>
      <TableCell className="text-xs">
        {providerKey.last_test ? (
          <div className="flex flex-col">
            <span
              className={
                providerKey.last_test.ok ? "text-green-700" : "text-red-600"
              }
            >
              {providerKey.last_test.message}
            </span>
            <span className="text-fg-muted">
              {formatTimestamp(providerKey.last_test.tested_at_ms)}
            </span>
          </div>
        ) : (
          <span className="text-fg-muted">Not tested</span>
        )}
      </TableCell>
      <TableCell>
        {canManage ? (
          <div className="flex flex-col gap-2">
            <Form
              method="post"
              className="flex items-end gap-2"
              preventScrollReset
              key={`${providerKey.name}:${providerKey.updated_at_ms ?? 0}`}
            >
              <input type="hidden" name="intent" value="set_key" />
              <input type="hidden" name="name" value={providerKey.name} />
              <div className="flex flex-col gap-1">
                <Label htmlFor={`key-${providerKey.name}`} className="sr-only">
                  {providerKey.label} API key
                </Label>
                <Input
                  id={`key-${providerKey.name}`}
                  name="api_key"
                  type="password"
                  autoComplete="off"
                  required
                  minLength={8}
                  placeholder={
                    providerKey.stored
                      ? "Paste a new key to rotate"
                      : "Paste API key"
                  }
                  className="w-64 font-mono"
                />
              </div>
              <Button type="submit" size="sm" disabled={busy}>
                <KeyRound aria-hidden />
                {providerKey.stored ? "Rotate" : "Save"}
              </Button>
            </Form>
            <div className="flex gap-2">
              <Form method="post" preventScrollReset>
                <input type="hidden" name="intent" value="test_key" />
                <input type="hidden" name="name" value={providerKey.name} />
                <Button
                  type="submit"
                  size="sm"
                  variant="outline"
                  disabled={busy || providerKey.source === "missing"}
                >
                  <PlugZap aria-hidden />
                  Test connection
                </Button>
              </Form>
              {providerKey.stored && (
                <Form method="post" preventScrollReset>
                  <input type="hidden" name="intent" value="delete_key" />
                  <input type="hidden" name="name" value={providerKey.name} />
                  <Button
                    type="submit"
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label={`Remove the stored ${providerKey.label} key`}
                  >
                    <Trash2 aria-hidden />
                    Remove
                  </Button>
                </Form>
              )}
            </div>
            {rowResult && (
              <p
                className={
                  rowResult.ok
                    ? "text-xs text-green-700"
                    : "text-xs text-red-600"
                }
                role={rowResult.ok ? "status" : "alert"}
              >
                {rowResult.ok ? rowResult.message : rowResult.error}
              </p>
            )}
          </div>
        ) : (
          <span className="text-fg-muted text-xs">Operator only</span>
        )}
      </TableCell>
    </TableRow>
  );
}

function ProviderKeysSection({ keys }: { keys: ProviderKeysState | null }) {
  if (!keys) {
    return (
      <SectionLayout>
        <SectionHeader heading="Provider API keys" />
        <p className="text-sm text-red-600" role="alert">
          Provider credentials could not be loaded from the gateway. Inference
          keeps using the keys it already has.
        </p>
      </SectionLayout>
    );
  }
  const canManage = keys.management_available && keys.operator;
  return (
    <SectionLayout>
      <SectionHeader heading="Provider API keys" count={keys.keys.length} />
      <p className="text-fg-tertiary text-sm">
        Keys stored here are encrypted in Postgres and take precedence over the
        gateway&apos;s environment variables; removing one falls back to the
        environment. Saved keys are never shown again, only their last four
        characters. Changes apply to new requests on every replica within 30
        seconds.
      </p>
      {keys.unavailable_reason && (
        <InlineNotice>{keys.unavailable_reason}</InlineNotice>
      )}
      {!keys.unavailable_reason && !keys.operator && (
        <InlineNotice>
          Only operator keys can manage provider credentials. Add this UI&apos;s
          API key id to RELAY_OPERATOR_KEY_IDS on the gateway.
        </InlineNotice>
      )}
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Provider</TableHead>
            <TableHead>Key</TableHead>
            <TableHead>Updated</TableHead>
            <TableHead>Last test</TableHead>
            <TableHead>Manage</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {keys.keys.map((providerKey) => (
            <ProviderKeyRow
              key={providerKey.name}
              providerKey={providerKey}
              canManage={canManage}
            />
          ))}
        </TableBody>
      </Table>
    </SectionLayout>
  );
}

function ProvidersContent({
  report,
  keys,
}: {
  report: ProviderHealthReport;
  keys: ProviderKeysState | null;
}) {
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
            rate, and opens a circuit breaker on a provider that keeps failing
            so traffic moves to the next route. Function-level failover (for
            example the Relay Chat fallback variant) is configured in the
            gateway config. Snapshot taken{" "}
            {formatTimestamp(report.generated_at_ms)}
            {openCircuits > 0 ? `; ${openCircuits} circuit(s) open.` : "."}
          </p>
        </SectionLayout>
        <ProviderKeysSection keys={keys} />
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
  const { page } = loaderData;
  const location = useLocation();
  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<ProvidersSkeleton />}>
        <Await resolve={page} errorElement={<ProvidersErrorState />}>
          {([resolvedReport, resolvedKeys]) => (
            <ProvidersContent report={resolvedReport} keys={resolvedKeys} />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
