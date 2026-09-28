import type { Route } from "./+types/route";
import { Link, type RouteHandle } from "react-router";
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
import { LayoutErrorBoundary } from "~/components/ui/error";
import { InlineNotice } from "~/components/billing/BillingShared";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { TraceScope } from "~/types/traces";
import {
  formatLatency,
  formatTokens,
  formatTraceCost,
  formatTraceTime,
  traceStatus,
} from "../format";

export const handle: RouteHandle = {
  crumb: (match) => [
    "Key Traces",
    { label: match.params.inference_id ?? "", isIdentifier: true },
  ],
};

export async function loader({ request, params }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope: TraceScope =
    new URL(request.url).searchParams.get("scope") === "all" ? "all" : "own";
  const detail = await getTensorZeroClient().getRelayTrace(
    params.inference_id,
    {
      scope,
    },
  );
  return { detail, scope };
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-fg-secondary text-xs">{label}</span>
      <span className="font-mono text-sm">{value}</span>
    </div>
  );
}

function JsonBlock({ value }: { value: unknown }) {
  return (
    <pre className="bg-bg-tertiary max-h-96 overflow-auto rounded-md p-3 text-xs">
      {JSON.stringify(value, null, 2)}
    </pre>
  );
}

export default function KeyTraceDetailPage({
  loaderData,
}: Route.ComponentProps) {
  const { detail, scope } = loaderData;
  const { summary } = detail;
  const back =
    scope === "all" ? "/observability/keys?scope=all" : "/observability/keys";
  return (
    <PageLayout>
      <PageHeader heading="Trace" />
      <Link
        to={back}
        className="text-fg-secondary hover:text-fg-primary text-sm underline-offset-4 hover:underline"
      >
        Back to traces
      </Link>
      <SectionsGroup>
        <SectionLayout>
          <SectionHeader heading="Summary" />
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Field
              label="Time (UTC)"
              value={formatTraceTime(summary.created_at_us)}
            />
            <Field
              label="API key"
              value={summary.api_key_public_id ?? "unauthenticated"}
            />
            <Field
              label="Function"
              value={`${summary.function_name} / ${summary.variant_name}`}
            />
            <Field
              label="Model"
              value={`${summary.model_name ?? "-"}${summary.model_provider_name ? ` / ${summary.model_provider_name}` : ""}`}
            />
            <Field
              label="Input tokens"
              value={formatTokens(summary.input_tokens)}
            />
            <Field
              label="Output tokens"
              value={formatTokens(summary.output_tokens)}
            />
            <Field
              label="Latency"
              value={formatLatency(summary.processing_time_ms)}
            />
            <Field label="Cost" value={formatTraceCost(summary.cost_micros)} />
            <Field label="Status" value={traceStatus(summary)} />
            <Field label="Inference" value={summary.inference_id} />
            <Field label="Episode" value={summary.episode_id} />
          </div>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Model calls" />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time (UTC)</TableHead>
                <TableHead>Model / provider</TableHead>
                <TableHead className="text-right">Input tokens</TableHead>
                <TableHead className="text-right">Output tokens</TableHead>
                <TableHead className="text-right">Latency</TableHead>
                <TableHead className="text-right">Cost</TableHead>
                <TableHead>Finish</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {detail.model_calls.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={7} className="text-fg-muted text-center">
                    No model call was stored for this trace.
                  </TableCell>
                </TableRow>
              ) : (
                detail.model_calls.map((call) => (
                  <TableRow key={call.id}>
                    <TableCell className="font-mono text-xs">
                      {formatTraceTime(call.created_at_us)}
                    </TableCell>
                    <TableCell className="font-mono text-xs">
                      {call.model_name} / {call.model_provider_name}
                      {call.cached ? " (cache)" : ""}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTokens(call.input_tokens)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTokens(call.output_tokens)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatLatency(call.response_time_ms)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTraceCost(call.cost_micros)}
                    </TableCell>
                    <TableCell>{call.finish_reason ?? "unknown"}</TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Prompt and output" />
          {detail.bodies_expired ? (
            <InlineNotice>
              The prompt and output were removed after the 14-day body retention
              window. The metadata above is kept for 90 days.
            </InlineNotice>
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              <div className="flex flex-col gap-2">
                <span className="text-fg-secondary text-sm">Input</span>
                <JsonBlock value={detail.input} />
              </div>
              <div className="flex flex-col gap-2">
                <span className="text-fg-secondary text-sm">Output</span>
                <JsonBlock value={detail.output} />
              </div>
            </div>
          )}
        </SectionLayout>
      </SectionsGroup>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
