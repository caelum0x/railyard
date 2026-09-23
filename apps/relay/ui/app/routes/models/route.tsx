import type { Route } from "./+types/route";
import { useMemo, useState } from "react";
import type { RouteHandle } from "react-router";
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
import type { CatalogModel } from "~/types/catalog";

const MICROS_PER_USD = 1_000_000;

export const handle: RouteHandle = {
  crumb: () => ["Models"],
};

export async function loader() {
  await requireValidApiKeyIfEnabled();
  const catalog = await getTensorZeroClient().getRelayCatalog();
  return { catalog };
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
      </SectionsGroup>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
