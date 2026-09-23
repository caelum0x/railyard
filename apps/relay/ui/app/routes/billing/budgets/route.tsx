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
import { Trash2 } from "lucide-react";
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
import { Button } from "~/components/ui/button";
import { Input } from "~/components/ui/input";
import { Label } from "~/components/ui/label";
import { Badge } from "~/components/ui/badge";
import { Skeleton } from "~/components/ui/skeleton";
import { LayoutErrorBoundary, PageErrorContent } from "~/components/ui/error";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { logger } from "~/utils/logger";
import {
  BillingStatusBanner,
  InlineNotice,
  MICROS_PER_USD,
  ScopeToggle,
  formatTimestamp,
  formatUsd,
  scopeFromUrl,
} from "~/components/billing/BillingShared";
import type {
  BillingScope,
  BillingStatus,
  KeyBudget,
} from "~/types/billing";

const MAX_NOTE_CHARS = 280;
const KEY_ID_PATTERN = /^[A-Za-z0-9_-]{1,128}$/;

export const handle: RouteHandle = {
  crumb: () => ["Billing", "Spend Caps"],
};

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const scope = scopeFromUrl(request.url);
  const client = getTensorZeroClient();
  const page = Promise.all([
    client.listBudgets({ scope }),
    client.getBillingStatus().catch((): BillingStatus | null => null),
  ]);
  return { page, scope };
}

type ActionResult =
  | { ok: true; message: string }
  | { ok: false; error: string; fieldErrors?: Record<string, string> };

/** Parse a USD amount like `25` or `12.50` into exact integer micro-USD. */
function parseUsdToMicros(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+(\.\d{1,6})?$/.test(trimmed)) {
    return null;
  }
  const [whole, fraction = ""] = trimmed.split(".");
  const micros =
    Number(whole) * MICROS_PER_USD + Number(fraction.padEnd(6, "0"));
  return Number.isSafeInteger(micros) && micros > 0 ? micros : null;
}

export async function action({ request }: Route.ActionArgs) {
  await requireValidApiKeyIfEnabled();
  const formData = await request.formData();
  const intent = formData.get("intent");
  const keyId = String(formData.get("keyId") ?? "").trim();
  if (!KEY_ID_PATTERN.test(keyId)) {
    return data<ActionResult>(
      {
        ok: false,
        error: "Enter the API key's public id.",
        fieldErrors: { keyId: "Use the public id shown on the API Keys page." },
      },
      { status: 400 },
    );
  }
  const client = getTensorZeroClient();

  if (intent === "delete") {
    try {
      await client.deleteBudget(keyId);
      return data<ActionResult>({
        ok: true,
        message: `Removed the spend cap for ${keyId}.`,
      });
    } catch (error) {
      logger.error("Failed to delete Relay spend cap", error);
      return data<ActionResult>(
        { ok: false, error: errorMessage(error) },
        { status: 502 },
      );
    }
  }

  if (intent !== "upsert") {
    return data<ActionResult>(
      { ok: false, error: "Unknown action." },
      { status: 400 },
    );
  }
  const micros = parseUsdToMicros(String(formData.get("limitUsd") ?? ""));
  if (micros === null) {
    return data<ActionResult>(
      {
        ok: false,
        error: "Enter a positive monthly cap in USD.",
        fieldErrors: { limitUsd: "For example 25 or 12.50" },
      },
      { status: 400 },
    );
  }
  const note = String(formData.get("note") ?? "").trim();
  if (note.length > MAX_NOTE_CHARS) {
    return data<ActionResult>(
      {
        ok: false,
        error: `Notes are limited to ${MAX_NOTE_CHARS} characters.`,
        fieldErrors: { note: "Too long" },
      },
      { status: 400 },
    );
  }
  try {
    await client.upsertBudget(keyId, {
      monthly_limit_micros: micros,
      note: note || null,
    });
    return data<ActionResult>({
      ok: true,
      message: `Saved a ${formatUsd(micros)} monthly cap for ${keyId}.`,
    });
  } catch (error) {
    logger.error("Failed to save Relay spend cap", error);
    return data<ActionResult>(
      { ok: false, error: errorMessage(error) },
      { status: 502 },
    );
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error && error.message
    ? error.message
    : "The gateway rejected the request. Try again.";
}

function BudgetsPageHeader() {
  return <PageHeader heading="Spend Caps" />;
}

function BudgetsSkeleton() {
  return (
    <>
      <BudgetsPageHeader />
      <SectionLayout>
        <Skeleton className="h-16 w-full rounded-md" />
        <Skeleton className="h-48 w-full rounded-md" />
      </SectionLayout>
    </>
  );
}

function BudgetsErrorState() {
  const error = useAsyncError();
  return (
    <>
      <BudgetsPageHeader />
      <SectionLayout>
        <PageErrorContent error={error} />
      </SectionLayout>
    </>
  );
}

function UsageBar({ budget }: { budget: KeyBudget }) {
  const ratio =
    budget.monthly_limit_micros > 0
      ? Math.min(1, budget.month_to_date_micros / budget.monthly_limit_micros)
      : 0;
  const tone =
    ratio >= 1 ? "bg-red-500" : ratio >= 0.8 ? "bg-amber-500" : "bg-green-500";
  return (
    <div className="flex items-center gap-2">
      <div
        className="bg-bg-tertiary h-2 w-32 overflow-hidden rounded-full"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(ratio * 100)}
      >
        <div className={`${tone} h-full`} style={{ width: `${ratio * 100}%` }} />
      </div>
      <span className="text-fg-secondary text-xs">
        {Math.round(ratio * 100)}%
      </span>
    </div>
  );
}

function BudgetForm({ submitting }: { submitting: boolean }) {
  const actionData = useActionData<typeof action>();
  const fieldErrors =
    actionData && !actionData.ok ? (actionData.fieldErrors ?? {}) : {};
  return (
    <Form method="post" className="flex flex-wrap items-end gap-3" preventScrollReset>
      <input type="hidden" name="intent" value="upsert" />
      <div className="flex flex-col gap-1">
        <Label htmlFor="budget-key">API key public id</Label>
        <Input
          id="budget-key"
          name="keyId"
          required
          placeholder="abcdefghijkl"
          className="w-56 font-mono"
          aria-invalid={fieldErrors.keyId ? true : undefined}
        />
        {fieldErrors.keyId && (
          <span className="text-xs text-red-600">{fieldErrors.keyId}</span>
        )}
      </div>
      <div className="flex flex-col gap-1">
        <Label htmlFor="budget-limit">Monthly cap (USD)</Label>
        <Input
          id="budget-limit"
          name="limitUsd"
          required
          inputMode="decimal"
          placeholder="25.00"
          className="w-36"
          aria-invalid={fieldErrors.limitUsd ? true : undefined}
        />
        {fieldErrors.limitUsd && (
          <span className="text-xs text-red-600">{fieldErrors.limitUsd}</span>
        )}
      </div>
      <div className="flex min-w-64 flex-1 flex-col gap-1">
        <Label htmlFor="budget-note">Note (optional)</Label>
        <Input
          id="budget-note"
          name="note"
          maxLength={MAX_NOTE_CHARS}
          placeholder="AutoGTM production worker"
        />
      </div>
      <Button type="submit" disabled={submitting}>
        {submitting ? "Saving" : "Save cap"}
      </Button>
    </Form>
  );
}

function BudgetsContent({
  budgets,
  status,
  scope,
}: {
  budgets: KeyBudget[];
  status: BillingStatus | null;
  scope: BillingScope;
}) {
  const navigation = useNavigation();
  const actionData = useActionData<typeof action>();
  const submitting = navigation.state !== "idle";
  const operator = status?.operator ?? false;

  return (
    <>
      <BudgetsPageHeader />
      <SectionsGroup>
        <SectionLayout>
          {status && <BillingStatusBanner status={status} />}
          <p className="text-fg-secondary text-sm">
            A spend cap bounds an API key&apos;s charged spend per UTC calendar
            month. Once reached, the gateway answers new inference from that
            key with HTTP 429 <span className="font-mono">relay_spend_cap_exceeded</span>{" "}
            before contacting any provider, in both prepaid and postpaid modes.
          </p>
          <ScopeToggle scope={scope} operator={operator} />
          {actionData && (
            <p
              className={actionData.ok ? "text-sm text-green-700" : "text-sm text-red-600"}
              role={actionData.ok ? "status" : "alert"}
            >
              {actionData.ok ? actionData.message : actionData.error}
            </p>
          )}
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Set a cap" />
          {operator ? (
            <BudgetForm submitting={submitting} />
          ) : (
            <InlineNotice>
              Only operator keys can change spend caps. Add this UI&apos;s API
              key id to RELAY_OPERATOR_KEY_IDS on the gateway.
            </InlineNotice>
          )}
        </SectionLayout>

        <SectionLayout>
          <SectionHeader heading="Caps" count={budgets.length} />
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>API key</TableHead>
                <TableHead className="text-right">Monthly cap</TableHead>
                <TableHead className="text-right">Month to date</TableHead>
                <TableHead className="text-right">Remaining</TableHead>
                <TableHead>Usage</TableHead>
                <TableHead>Updated</TableHead>
                <TableHead className="w-0"></TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {budgets.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={7} className="text-fg-muted text-center">
                    No spend caps configured. Keys without a cap are unlimited.
                  </TableCell>
                </TableRow>
              ) : (
                budgets.map((budget) => (
                  <TableRow key={budget.owner_principal}>
                    <TableCell>
                      <div className="flex flex-col">
                        <span className="font-mono">{budget.owner_principal}</span>
                        {budget.note && (
                          <span className="text-fg-muted text-xs">{budget.note}</span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(budget.monthly_limit_micros)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(budget.month_to_date_micros)}
                    </TableCell>
                    <TableCell className="text-right">
                      {formatUsd(budget.remaining_micros)}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-2">
                        <UsageBar budget={budget} />
                        {budget.exceeded && (
                          <Badge variant="destructive">Blocked</Badge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-xs">
                      <div className="flex flex-col">
                        <span>{formatTimestamp(budget.updated_at_ms)}</span>
                        <span className="text-fg-muted font-mono">
                          {budget.updated_by}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      {operator && (
                        <Form method="post" preventScrollReset>
                          <input type="hidden" name="intent" value="delete" />
                          <input
                            type="hidden"
                            name="keyId"
                            value={budget.owner_principal}
                          />
                          <Button
                            type="submit"
                            variant="ghost"
                            size="iconSm"
                            disabled={submitting}
                            aria-label={`Remove cap for ${budget.owner_principal}`}
                          >
                            <Trash2 aria-hidden />
                          </Button>
                        </Form>
                      )}
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </SectionLayout>
      </SectionsGroup>
    </>
  );
}

export default function SpendCapsPage({ loaderData }: Route.ComponentProps) {
  const { page, scope } = loaderData;
  const location = useLocation();

  return (
    <PageLayout>
      <Suspense key={location.key} fallback={<BudgetsSkeleton />}>
        <Await resolve={page} errorElement={<BudgetsErrorState />}>
          {([budgets, status]) => (
            <BudgetsContent budgets={budgets} status={status} scope={scope} />
          )}
        </Await>
      </Suspense>
    </PageLayout>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  return <LayoutErrorBoundary error={error} />;
}
