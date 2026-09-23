import type { Route } from "./+types/route";
import { useMemo, useState } from "react";
import {
  Form,
  data,
  useActionData,
  useNavigation,
  type RouteHandle,
} from "react-router";
import { Calculator } from "lucide-react";
import { Button } from "~/components/ui/button";
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
import { Input } from "~/components/ui/input";
import { LayoutErrorBoundary } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import type { CatalogModel, PriceQuote } from "~/types/catalog";

const MICROS_PER_USD = 1_000_000;

export const handle: RouteHandle = {
  crumb: () => ["Models"],
};

export async function loader() {
  await requireValidApiKeyIfEnabled();
  const catalog = await getTensorZeroClient().getRelayCatalog();
  return { catalog };
}

const MAX_QUOTE_TOKENS = 100_000_000;

type QuoteResult =
  | { ok: true; quote: PriceQuote }
  | { ok: false; error: string };

function parseTokenCount(raw: FormDataEntryValue | null): number | null {
  const value = Number(String(raw ?? "").trim());
  return Number.isInteger(value) && value >= 0 && value <= MAX_QUOTE_TOKENS
    ? value
    : null;
}

export async function action({ request }: Route.ActionArgs) {
  await requireValidApiKeyIfEnabled();
  const formData = await request.formData();
  const model = String(formData.get("model") ?? "").trim();
  const inputTokens = parseTokenCount(formData.get("input_tokens"));
  const outputTokens = parseTokenCount(formData.get("output_tokens"));
  if (!model || inputTokens === null || outputTokens === null) {
    return data<QuoteResult>(
      {
        ok: false,
        error: `Choose a model and enter whole token counts between 0 and ${MAX_QUOTE_TOKENS.toLocaleString()}.`,
      },
      { status: 400 },
    );
  }
  try {
    const quote = await getTensorZeroClient().getRelayQuote({
      model,
      input_tokens: inputTokens,
      output_tokens: outputTokens,
    });
    return data<QuoteResult>({ ok: true, quote });
  } catch (error) {
    logger.error("Relay quote failed", error);
    return data<QuoteResult>(
      {
        ok: false,
        error:
          error instanceof Error && error.message
            ? error.message
            : "The gateway could not price this request.",
      },
      { status: 502 },
    );
  }
}

function formatQuoteUsd(micros: number): string {
  return (micros / MICROS_PER_USD).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

function QuoteCalculator({ models }: { models: CatalogModel[] }) {
  const actionData = useActionData<typeof action>();
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  const enabled = models.filter((model) => model.enabled);
  return (
    <SectionLayout>
      <SectionHeader heading="Price calculator" />
      <p className="text-fg-tertiary text-sm">
        Estimate the retail charge for a request from the live catalog. The
        gateway prices with the same integer micro-USD math used at settlement.
      </p>
      <Form method="post" className="flex flex-wrap items-end gap-3" preventScrollReset>
        <div className="flex flex-col gap-1">
          <Label htmlFor="quote-model">Model</Label>
          <select
            id="quote-model"
            name="model"
            required
            defaultValue={enabled[0]?.id}
            className="border-input bg-background h-9 max-w-md rounded-md border px-3 text-sm"
          >
            {enabled.map((model) => (
              <option key={model.id} value={model.id}>
                {model.display_name} ({model.id})
              </option>
            ))}
          </select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="quote-input">Input tokens</Label>
          <Input
            id="quote-input"
            name="input_tokens"
            type="number"
            min={0}
            max={MAX_QUOTE_TOKENS}
            step={1}
            defaultValue={1000}
            required
            className="w-36"
          />
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="quote-output">Output tokens</Label>
          <Input
            id="quote-output"
            name="output_tokens"
            type="number"
            min={0}
            max={MAX_QUOTE_TOKENS}
            step={1}
            defaultValue={500}
            required
            className="w-36"
          />
        </div>
        <Button type="submit" disabled={submitting || enabled.length === 0}>
          <Calculator aria-hidden />
          {submitting ? "Pricing" : "Estimate"}
        </Button>
      </Form>
      {actionData && !actionData.ok && (
        <p className="text-sm text-red-600" role="alert">
          {actionData.error}
        </p>
      )}
      {actionData && actionData.ok && (
        <div className="border-border bg-bg-secondary flex flex-wrap items-baseline gap-x-6 gap-y-1 rounded-md border p-4 text-sm">
          <span className="text-fg-primary text-xl font-medium">
            {formatQuoteUsd(actionData.quote.amount_micros)}
          </span>
          <span className="font-mono">{actionData.quote.model}</span>
          <span>
            {actionData.quote.input_tokens.toLocaleString()} in /{" "}
            {actionData.quote.output_tokens.toLocaleString()} out
          </span>
          <span className="text-fg-muted">
            Catalog revision {actionData.quote.catalog_revision}
          </span>
        </div>
      )}
    </SectionLayout>
  );
}

function formatPricePerMillion(micros: number): string {
  return (micros / MICROS_PER_USD).toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 4,
  });
}

function formatTokens(value: number | undefined): string {
  if (value == null) {
    return "—";
  }
  return value.toLocaleString();
}

function capabilityLabels(model: CatalogModel): string[] {
  const caps = model.capabilities;
  const labels: string[] = [];
  if (caps.chat) labels.push("Chat");
  if (caps.embeddings) labels.push("Embeddings");
  if (caps.tools) labels.push("Tools");
  if (caps.vision) labels.push("Vision");
  if (caps.json_schema) labels.push("JSON");
  if (caps.streaming) labels.push("Streaming");
  if (caps.batch) labels.push("Batch");
  return labels;
}

export default function ModelsCatalogPage({
  loaderData,
}: Route.ComponentProps) {
  const { catalog } = loaderData;
  const [query, setQuery] = useState("");

  const providers = useMemo(() => {
    const set = new Set(catalog.models.map((model) => model.provider));
    return [...set].sort();
  }, [catalog.models]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const sorted = [...catalog.models].sort((left, right) =>
      left.id.localeCompare(right.id),
    );
    if (!needle) {
      return sorted;
    }
    return sorted.filter((model) => {
      const haystack = [
        model.id,
        model.display_name,
        model.provider,
        ...model.aliases,
      ]
        .join(" ")
        .toLowerCase();
      return haystack.includes(needle);
    });
  }, [catalog.models, query]);

  return (
    <PageLayout>
      <PageHeader
        heading="Model Catalog"
        tag={
          <Badge variant="secondary">
            {catalog.models.length.toLocaleString()} models
          </Badge>
        }
      />
      <SectionsGroup>
        <SectionLayout>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <SectionHeader heading="Configured models" />
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search models or providers"
              aria-label="Search models"
              className="max-w-xs"
            />
          </div>
          <p className="text-fg-tertiary text-sm">
            Revision{" "}
            <span className="font-mono">{catalog.revision}</span> across{" "}
            {providers.length.toLocaleString()} provider
            {providers.length === 1 ? "" : "s"}.
          </p>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Model</TableHead>
                <TableHead>Provider</TableHead>
                <TableHead>Capabilities</TableHead>
                <TableHead className="text-right">Input / 1M</TableHead>
                <TableHead className="text-right">Output / 1M</TableHead>
                <TableHead className="text-right">Context</TableHead>
                <TableHead className="text-center">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {filtered.length === 0 ? (
                <TableRow>
                  <TableCell
                    colSpan={7}
                    className="text-fg-muted text-center"
                  >
                    No models match your search.
                  </TableCell>
                </TableRow>
              ) : (
                filtered.map((model) => (
                  <TableRow key={model.id}>
                    <TableCell>
                      <div className="font-medium">{model.display_name}</div>
                      <div className="text-fg-muted font-mono text-xs">
                        {model.id}
                      </div>
                    </TableCell>
                    <TableCell>{model.provider}</TableCell>
                    <TableCell>
                      <div className="flex flex-wrap gap-1">
                        {capabilityLabels(model).map((label) => (
                          <Badge key={label} variant="outline">
                            {label}
                          </Badge>
                        ))}
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      {formatPricePerMillion(
                        model.pricing.input_micro_usd_per_million,
                      )}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatPricePerMillion(
                        model.pricing.output_micro_usd_per_million,
                      )}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatTokens(model.context_window_tokens)}
                    </TableCell>
                    <TableCell className="text-center">
                      {model.enabled ? (
                        <Badge variant="secondary">Enabled</Badge>
                      ) : (
                        <Badge variant="outline">Disabled</Badge>
                      )}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>
        <QuoteCalculator models={catalog.models} />
        <SectionLayout>
          <p className="text-fg-tertiary text-sm">
            Retail prices come from the gateway&apos;s catalog file
            (RELAY_CATALOG_PATH) and must cover every model and function in the
            gateway config. Edit both and redeploy to add a model or change a
            price.
          </p>
        </SectionLayout>
      </SectionsGroup>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
