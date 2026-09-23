import type { Route } from "./+types/route";
import { useMemo, useRef, useState, type RefObject } from "react";
import {
  Form,
  data,
  useActionData,
  useNavigation,
  type RouteHandle,
} from "react-router";
import { Calculator, Pencil, RotateCcw } from "lucide-react";
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
import type {
  CatalogModel,
  PriceEdit,
  PriceEditorState,
  PriceQuote,
} from "~/types/catalog";
import {
  InlineNotice,
  formatTimestamp,
} from "~/components/billing/BillingShared";

const MICROS_PER_USD = 1_000_000;

export const handle: RouteHandle = {
  crumb: () => ["Models"],
};

export async function loader() {
  await requireValidApiKeyIfEnabled();
  const client = getTensorZeroClient();
  const [catalog, priceEditor] = await Promise.all([
    client.getRelayCatalog(),
    client.getPriceEditor().catch((error: unknown) => {
      logger.error("Relay price editor state unavailable", error);
      return null;
    }),
  ]);
  return { catalog, priceEditor };
}

const MAX_PRICE_NOTE_CHARS = 280;
/** $1,000,000 per million tokens, mirroring the gateway's upper bound. */
const MAX_PRICE_MICROS_PER_MILLION = 1_000_000_000_000;

const MAX_QUOTE_TOKENS = 100_000_000;

type QuoteResult =
  | { kind: "quote"; ok: true; quote: PriceQuote }
  | { kind: "quote"; ok: false; error: string };

type PriceResult =
  | { kind: "price"; ok: true; message: string }
  | {
      kind: "price";
      ok: false;
      error: string;
      fieldErrors?: Record<string, string>;
    };

type ModelsActionResult = QuoteResult | PriceResult;

/** Parse dollars per million tokens (`0.15`, `15`) into integer micro-USD. */
function parseRatePerMillion(raw: FormDataEntryValue | null): number | null {
  const trimmed = String(raw ?? "")
    .trim()
    .replace(/^\$/, "");
  if (!/^\d+(\.\d{1,6})?$/.test(trimmed)) {
    return null;
  }
  const [whole, fraction = ""] = trimmed.split(".");
  const micros =
    Number(whole) * MICROS_PER_USD + Number(fraction.padEnd(6, "0"));
  return Number.isSafeInteger(micros) && micros <= MAX_PRICE_MICROS_PER_MILLION
    ? micros
    : null;
}

function actionErrorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

async function handlePriceAction(intent: string, formData: FormData) {
  const client = getTensorZeroClient();
  const model = String(formData.get("model") ?? "").trim();
  if (!model) {
    return data<PriceResult>(
      { kind: "price", ok: false, error: "Choose a model." },
      { status: 400 },
    );
  }
  if (intent === "reset_price") {
    try {
      await client.resetModelPrice(model);
      return data<PriceResult>({
        kind: "price",
        ok: true,
        message: `${model} is back on its deployed catalog price.`,
      });
    } catch (error) {
      logger.error("Relay price reset failed", error);
      return data<PriceResult>(
        {
          kind: "price",
          ok: false,
          error: actionErrorMessage(error, "The price could not be reset."),
        },
        { status: 502 },
      );
    }
  }
  const input = parseRatePerMillion(formData.get("input_usd"));
  const output = parseRatePerMillion(formData.get("output_usd"));
  const fieldErrors: Record<string, string> = {};
  if (input === null)
    fieldErrors.input_usd = "Dollars per 1M tokens, e.g. 0.15";
  if (output === null)
    fieldErrors.output_usd = "Dollars per 1M tokens, e.g. 0.60";
  const note = String(formData.get("note") ?? "").trim();
  if (note.length > MAX_PRICE_NOTE_CHARS) fieldErrors.note = "Too long";
  if (input === null || output === null || fieldErrors.note) {
    return data<PriceResult>(
      {
        kind: "price",
        ok: false,
        error: "Fix the highlighted fields.",
        fieldErrors,
      },
      { status: 400 },
    );
  }
  try {
    const updated = await client.setModelPrice({
      model,
      input_micro_usd_per_million: input,
      output_micro_usd_per_million: output,
      note: note || null,
    });
    return data<PriceResult>({
      kind: "price",
      ok: true,
      message: `${updated.display_name} now bills ${formatPricePerMillion(
        updated.pricing.input_micro_usd_per_million,
      )} in / ${formatPricePerMillion(
        updated.pricing.output_micro_usd_per_million,
      )} out per 1M tokens. New requests use this price immediately.`,
    });
  } catch (error) {
    logger.error("Relay price update failed", error);
    return data<PriceResult>(
      {
        kind: "price",
        ok: false,
        error: actionErrorMessage(error, "The price could not be saved."),
      },
      { status: 502 },
    );
  }
}

function parseTokenCount(raw: FormDataEntryValue | null): number | null {
  const value = Number(String(raw ?? "").trim());
  return Number.isInteger(value) && value >= 0 && value <= MAX_QUOTE_TOKENS
    ? value
    : null;
}

export async function action({ request }: Route.ActionArgs) {
  await requireValidApiKeyIfEnabled();
  const formData = await request.formData();
  const intent = String(formData.get("intent") ?? "quote");
  if (intent === "set_price" || intent === "reset_price") {
    return handlePriceAction(intent, formData);
  }
  const model = String(formData.get("model") ?? "").trim();
  const inputTokens = parseTokenCount(formData.get("input_tokens"));
  const outputTokens = parseTokenCount(formData.get("output_tokens"));
  if (!model || inputTokens === null || outputTokens === null) {
    return data<QuoteResult>(
      {
        kind: "quote",
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
    return data<QuoteResult>({ kind: "quote", ok: true, quote });
  } catch (error) {
    logger.error("Relay quote failed", error);
    return data<QuoteResult>(
      {
        kind: "quote",
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
  const result = useActionData<typeof action>() as
    | ModelsActionResult
    | undefined;
  const actionData = result?.kind === "quote" ? result : undefined;
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
      <Form
        method="post"
        className="flex flex-wrap items-end gap-3"
        preventScrollReset
      >
        <input type="hidden" name="intent" value="quote" />
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

function usdInputValue(micros: number): string {
  return String(micros / MICROS_PER_USD);
}

function PriceEditorForm({
  models,
  selected,
  onSelect,
}: {
  models: CatalogModel[];
  selected: string;
  onSelect: (id: string) => void;
}) {
  const result = useActionData<typeof action>() as
    | ModelsActionResult
    | undefined;
  const priceResult = result?.kind === "price" ? result : undefined;
  const fieldErrors =
    priceResult && !priceResult.ok ? (priceResult.fieldErrors ?? {}) : {};
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  const model = models.find((candidate) => candidate.id === selected);
  return (
    <div className="flex flex-col gap-2">
      <Form
        method="post"
        className="flex flex-wrap items-end gap-3"
        preventScrollReset
        key={`${selected}:${model?.pricing.input_micro_usd_per_million}:${model?.pricing.output_micro_usd_per_million}`}
      >
        <input type="hidden" name="intent" value="set_price" />
        <div className="flex flex-col gap-1">
          <Label htmlFor="price-model">Model</Label>
          <select
            id="price-model"
            name="model"
            required
            value={selected}
            onChange={(event) => onSelect(event.target.value)}
            className="border-input bg-background h-9 max-w-md rounded-md border px-3 text-sm"
          >
            {models.map((candidate) => (
              <option key={candidate.id} value={candidate.id}>
                {candidate.display_name} ({candidate.id})
              </option>
            ))}
          </select>
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="price-input">Input USD / 1M tokens</Label>
          <Input
            id="price-input"
            name="input_usd"
            required
            inputMode="decimal"
            defaultValue={
              model
                ? usdInputValue(model.pricing.input_micro_usd_per_million)
                : ""
            }
            className="w-40"
            aria-invalid={fieldErrors.input_usd ? true : undefined}
          />
          {fieldErrors.input_usd && (
            <span className="text-xs text-red-600">
              {fieldErrors.input_usd}
            </span>
          )}
        </div>
        <div className="flex flex-col gap-1">
          <Label htmlFor="price-output">Output USD / 1M tokens</Label>
          <Input
            id="price-output"
            name="output_usd"
            required
            inputMode="decimal"
            defaultValue={
              model
                ? usdInputValue(model.pricing.output_micro_usd_per_million)
                : ""
            }
            className="w-40"
            aria-invalid={fieldErrors.output_usd ? true : undefined}
          />
          {fieldErrors.output_usd && (
            <span className="text-xs text-red-600">
              {fieldErrors.output_usd}
            </span>
          )}
        </div>
        <div className="flex min-w-64 flex-1 flex-col gap-1">
          <Label htmlFor="price-note">Note (optional)</Label>
          <Input
            id="price-note"
            name="note"
            maxLength={MAX_PRICE_NOTE_CHARS}
            placeholder="Matched OpenAI's October list price"
          />
        </div>
        <Button type="submit" disabled={submitting || models.length === 0}>
          {submitting ? "Saving" : "Save price"}
        </Button>
      </Form>
      {priceResult && (
        <p
          className={
            priceResult.ok ? "text-sm text-green-700" : "text-sm text-red-600"
          }
          role={priceResult.ok ? "status" : "alert"}
        >
          {priceResult.ok ? priceResult.message : priceResult.error}
        </p>
      )}
    </div>
  );
}

function PriceEditsTable({
  edits,
  canEdit,
}: {
  edits: PriceEdit[];
  canEdit: boolean;
}) {
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Model</TableHead>
          <TableHead className="text-right">Input / 1M</TableHead>
          <TableHead className="text-right">Output / 1M</TableHead>
          <TableHead className="text-right">Deployed price</TableHead>
          <TableHead>Changed</TableHead>
          <TableHead className="w-0"></TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {edits.length === 0 ? (
          <TableRow>
            <TableCell colSpan={6} className="text-fg-muted text-center">
              No price edits. Every model bills at its deployed catalog price.
            </TableCell>
          </TableRow>
        ) : (
          edits.map((edit) => (
            <TableRow key={edit.model_id}>
              <TableCell>
                <div className="font-mono text-xs">{edit.model_id}</div>
                {edit.note && (
                  <div className="text-fg-muted text-xs">{edit.note}</div>
                )}
              </TableCell>
              <TableCell className="text-right">
                {formatPricePerMillion(edit.input_micro_usd_per_million)}
              </TableCell>
              <TableCell className="text-right">
                {formatPricePerMillion(edit.output_micro_usd_per_million)}
              </TableCell>
              <TableCell className="text-fg-muted text-right text-xs">
                {edit.deployed_input_micro_usd_per_million != null &&
                edit.deployed_output_micro_usd_per_million != null
                  ? `${formatPricePerMillion(
                      edit.deployed_input_micro_usd_per_million,
                    )} / ${formatPricePerMillion(
                      edit.deployed_output_micro_usd_per_million,
                    )}`
                  : "Not in the deployed catalog"}
              </TableCell>
              <TableCell className="text-xs">
                <div>{formatTimestamp(edit.updated_at_ms)}</div>
                <div className="text-fg-muted font-mono">{edit.updated_by}</div>
              </TableCell>
              <TableCell>
                {canEdit && (
                  <Form method="post" preventScrollReset>
                    <input type="hidden" name="intent" value="reset_price" />
                    <input type="hidden" name="model" value={edit.model_id} />
                    <Button
                      type="submit"
                      variant="ghost"
                      size="sm"
                      disabled={submitting}
                      aria-label={`Reset ${edit.model_id} to its deployed price`}
                    >
                      <RotateCcw aria-hidden />
                      Reset
                    </Button>
                  </Form>
                )}
              </TableCell>
            </TableRow>
          ))
        )}
      </TableBody>
    </Table>
  );
}

function PriceEditorSection({
  models,
  editor,
  selected,
  onSelect,
  formRef,
}: {
  models: CatalogModel[];
  editor: PriceEditorState | null;
  selected: string;
  onSelect: (id: string) => void;
  formRef: RefObject<HTMLDivElement | null>;
}) {
  if (!editor) {
    return (
      <SectionLayout>
        <SectionHeader heading="Retail prices" />
        <p className="text-sm text-red-600" role="alert">
          Price edits could not be loaded from the gateway. The catalog above
          still shows the prices in effect.
        </p>
      </SectionLayout>
    );
  }
  const canEdit = editor.storage_available && editor.operator;
  return (
    <SectionLayout>
      <div ref={formRef}>
        <SectionHeader heading="Retail prices" count={editor.edits.length} />
      </div>
      <p className="text-fg-tertiary text-sm">
        Prices start from the deployed catalog (revision{" "}
        <span className="font-mono">{editor.deployed_revision}</span>). An edit
        takes effect for new requests immediately on this gateway and within 30
        seconds on every other replica, for both prepaid settlement and postpaid
        metering. Reset an edit to return to the deployed price.
      </p>
      {editor.unavailable_reason ? (
        <InlineNotice>{editor.unavailable_reason}</InlineNotice>
      ) : !editor.operator ? (
        <InlineNotice>
          Only operator keys can change prices. Add this UI&apos;s API key id to
          RELAY_OPERATOR_KEY_IDS on the gateway.
        </InlineNotice>
      ) : (
        <PriceEditorForm
          models={models}
          selected={selected}
          onSelect={onSelect}
        />
      )}
      <PriceEditsTable edits={editor.edits} canEdit={canEdit} />
    </SectionLayout>
  );
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
  const { catalog, priceEditor } = loaderData;
  const [query, setQuery] = useState("");
  const sortedModels = useMemo(
    () =>
      [...catalog.models].sort((left, right) =>
        left.id.localeCompare(right.id),
      ),
    [catalog.models],
  );
  const [priceModel, setPriceModel] = useState(sortedModels[0]?.id ?? "");
  const priceFormRef = useRef<HTMLDivElement | null>(null);
  const canEditPrices =
    (priceEditor?.storage_available ?? false) &&
    (priceEditor?.operator ?? false);
  const editedModels = useMemo(
    () => new Set(priceEditor?.edits.map((edit) => edit.model_id) ?? []),
    [priceEditor],
  );
  const startEditing = (id: string) => {
    setPriceModel(id);
    priceFormRef.current?.scrollIntoView({
      behavior: "smooth",
      block: "start",
    });
  };

  const providers = useMemo(() => {
    const set = new Set(catalog.models.map((model) => model.provider));
    return [...set].sort();
  }, [catalog.models]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) {
      return sortedModels;
    }
    return sortedModels.filter((model) => {
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
  }, [sortedModels, query]);

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
            Revision <span className="font-mono">{catalog.revision}</span>{" "}
            across {providers.length.toLocaleString()} provider
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
                  <TableCell colSpan={7} className="text-fg-muted text-center">
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
                      <div className="flex items-center justify-end gap-1">
                        {formatPricePerMillion(
                          model.pricing.output_micro_usd_per_million,
                        )}
                        {editedModels.has(model.id) && (
                          <Badge variant="outline">Edited</Badge>
                        )}
                        {canEditPrices && (
                          <Button
                            type="button"
                            variant="ghost"
                            size="iconSm"
                            onClick={() => startEditing(model.id)}
                            aria-label={`Edit the price of ${model.display_name}`}
                          >
                            <Pencil aria-hidden />
                          </Button>
                        )}
                      </div>
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
        <PriceEditorSection
          models={sortedModels}
          editor={priceEditor}
          selected={priceModel}
          onSelect={setPriceModel}
          formRef={priceFormRef}
        />
        <QuoteCalculator models={catalog.models} />
        <SectionLayout>
          <p className="text-fg-tertiary text-sm">
            The deployed catalog (RELAY_CATALOG_PATH) lists every model and
            function the gateway serves and their starting prices. Change prices
            above; adding a new model also needs a gateway config entry, so it
            is done in the catalog file and config together.
          </p>
        </SectionLayout>
      </SectionsGroup>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
