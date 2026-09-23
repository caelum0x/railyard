/**
 * Prepaid wallet management for the Billing page: live Mint balance, a
 * self-serve top-up (provider-hosted checkout) and, for operator keys, wallet
 * lookup plus manual credit from the Relay treasury account.
 *
 * All mutations post to the Billing route action (`intent` = `top_up` or
 * `credit`); this component only renders forms and their results.
 */
import { Form, useActionData, useNavigation } from "react-router";
import { Button } from "~/components/ui/button";
import { Input } from "~/components/ui/input";
import { Label } from "~/components/ui/label";
import { SectionHeader, SectionLayout } from "~/components/layout/PageLayout";
import {
  InlineNotice,
  StatCard,
  formatUsd,
} from "~/components/billing/BillingShared";
import type { WalletBalance } from "~/types/billing";

export type WalletActionResult =
  | { ok: true; intent: "credit"; message: string }
  | {
      ok: false;
      intent: "top_up" | "credit";
      error: string;
      fieldErrors?: Record<string, string>;
      /** Echoed so a retried credit reuses the same idempotency key. */
      idempotencyKey?: string;
    };

const PRESET_TOP_UPS_USD = [10, 25, 100] as const;

function FieldError({ message }: { message?: string }) {
  return message ? (
    <span className="text-xs text-red-600">{message}</span>
  ) : null;
}

function ResultMessage({
  result,
  intent,
}: {
  result: WalletActionResult | undefined;
  intent: "top_up" | "credit";
}) {
  if (!result || result.intent !== intent) {
    return null;
  }
  return (
    <p
      className={result.ok ? "text-sm text-green-700" : "text-sm text-red-600"}
      role={result.ok ? "status" : "alert"}
    >
      {result.ok ? result.message : result.error}
    </p>
  );
}

function TopUpForm({
  wallet,
  submitting,
  result,
}: {
  wallet: WalletBalance;
  submitting: boolean;
  result: WalletActionResult | undefined;
}) {
  const fieldErrors =
    result && !result.ok && result.intent === "top_up"
      ? (result.fieldErrors ?? {})
      : {};
  if (wallet.top_up_providers.length === 0) {
    return (
      <InlineNotice>
        Self-serve top-ups are not configured. Set RELAY_TOPUP_PROVIDERS on the
        gateway to the Mint checkout providers to offer (for example{" "}
        <span className="font-mono">creem</span>) and activate them in Mint.
      </InlineNotice>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <Form
        method="post"
        className="flex flex-wrap items-end gap-3"
        preventScrollReset
      >
        <input type="hidden" name="intent" value="top_up" />
        <input type="hidden" name="principal" value={wallet.principal} />
        <div className="flex flex-col gap-1">
          <Label htmlFor="topup-amount">Amount (USD)</Label>
          <Input
            id="topup-amount"
            name="amountUsd"
            required
            inputMode="decimal"
            placeholder="25.00"
            className="w-36"
            aria-invalid={fieldErrors.amountUsd ? true : undefined}
          />
          <FieldError message={fieldErrors.amountUsd} />
        </div>
        {wallet.top_up_providers.length > 1 ? (
          <div className="flex flex-col gap-1">
            <Label htmlFor="topup-provider">Pay with</Label>
            <select
              id="topup-provider"
              name="provider"
              className="border-border bg-bg-primary h-9 rounded-md border px-2 text-sm"
              defaultValue={wallet.top_up_providers[0]}
            >
              {wallet.top_up_providers.map((provider) => (
                <option key={provider} value={provider}>
                  {provider}
                </option>
              ))}
            </select>
          </div>
        ) : (
          <input
            type="hidden"
            name="provider"
            value={wallet.top_up_providers[0]}
          />
        )}
        <Button type="submit" disabled={submitting}>
          {submitting ? "Opening checkout" : "Add funds"}
        </Button>
      </Form>
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-fg-muted text-xs">Quick amounts:</span>
        {PRESET_TOP_UPS_USD.map((amount) => (
          <Form key={amount} method="post" preventScrollReset>
            <input type="hidden" name="intent" value="top_up" />
            <input type="hidden" name="principal" value={wallet.principal} />
            <input type="hidden" name="amountUsd" value={String(amount)} />
            <input
              type="hidden"
              name="provider"
              value={wallet.top_up_providers[0]}
            />
            <Button
              type="submit"
              variant="outline"
              size="sm"
              disabled={submitting}
            >
              ${amount}
            </Button>
          </Form>
        ))}
      </div>
      <p className="text-fg-muted text-xs">
        Between {formatUsd(wallet.min_top_up_micros)} and{" "}
        {formatUsd(wallet.max_top_up_micros)}. You are sent to the payment
        provider; the balance updates once the provider confirms the payment.
      </p>
      <ResultMessage result={result} intent="top_up" />
    </div>
  );
}

function CreditForm({
  wallet,
  submitting,
  result,
  idempotencyKey,
}: {
  wallet: WalletBalance;
  submitting: boolean;
  result: WalletActionResult | undefined;
  idempotencyKey: string;
}) {
  const failed = result && !result.ok && result.intent === "credit";
  const fieldErrors = failed ? (result.fieldErrors ?? {}) : {};
  const key = (failed && result.idempotencyKey) || idempotencyKey;
  return (
    <div className="flex flex-col gap-2">
      <Form
        method="post"
        className="flex flex-wrap items-end gap-3"
        preventScrollReset
      >
        <input type="hidden" name="intent" value="credit" />
        <input type="hidden" name="principal" value={wallet.principal} />
        <input type="hidden" name="idempotencyKey" value={key} />
        <div className="flex flex-col gap-1">
          <Label htmlFor="credit-amount">Credit (USD)</Label>
          <Input
            id="credit-amount"
            name="amountUsd"
            required
            inputMode="decimal"
            placeholder="50.00"
            className="w-36"
            aria-invalid={fieldErrors.amountUsd ? true : undefined}
          />
          <FieldError message={fieldErrors.amountUsd} />
        </div>
        <div className="flex min-w-64 flex-1 flex-col gap-1">
          <Label htmlFor="credit-note">
            Reason (recorded on the Mint transfer)
          </Label>
          <Input
            id="credit-note"
            name="note"
            maxLength={280}
            placeholder="Invoice INV-1042 prepayment"
            aria-invalid={fieldErrors.note ? true : undefined}
          />
          <FieldError message={fieldErrors.note} />
        </div>
        <Button type="submit" variant="secondary" disabled={submitting}>
          {submitting ? "Crediting" : "Credit wallet"}
        </Button>
      </Form>
      <p className="text-fg-muted text-xs">
        Moves funds from the treasury account (RELAY_WALLET_TREASURY_ACCOUNT)
        into {wallet.principal}&apos;s payer account. Resubmitting the same form
        never credits twice.
      </p>
      <ResultMessage result={result} intent="credit" />
    </div>
  );
}

function WalletLookup({ principal }: { principal: string }) {
  return (
    <Form method="get" className="flex flex-wrap items-end gap-3">
      <div className="flex flex-col gap-1">
        <Label htmlFor="wallet-principal">API key public id</Label>
        <Input
          id="wallet-principal"
          name="wallet"
          defaultValue={principal}
          placeholder="abcdefghijkl"
          className="w-56 font-mono"
          pattern="[A-Za-z0-9_\-]{1,128}"
        />
      </div>
      <Button type="submit" variant="outline">
        Open wallet
      </Button>
    </Form>
  );
}

export function WalletPanel({
  wallet,
  operator,
  idempotencyKey,
  topUpReturn,
}: {
  wallet: WalletBalance | null;
  operator: boolean;
  idempotencyKey: string;
  topUpReturn: "success" | "cancelled" | null;
}) {
  const navigation = useNavigation();
  const result = useActionData() as WalletActionResult | undefined;
  const submitting = navigation.state === "submitting";

  if (!wallet) {
    return (
      <SectionLayout>
        <SectionHeader heading="Wallet" />
        <p className="text-sm text-red-600" role="alert">
          The Mint wallet balance is unavailable right now. Spend figures below
          are still current; try again shortly.
        </p>
      </SectionLayout>
    );
  }
  if (!wallet.billing_enabled) {
    return null;
  }

  return (
    <SectionLayout>
      <SectionHeader heading="Wallet" />
      {topUpReturn === "success" && (
        <p className="text-sm text-green-700" role="status">
          Payment submitted. The balance updates as soon as the payment provider
          confirms it to Mint; refresh in a minute if it has not changed yet.
        </p>
      )}
      {topUpReturn === "cancelled" && (
        <p className="text-fg-secondary text-sm" role="status">
          Checkout cancelled. No payment was taken.
        </p>
      )}
      <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
        <StatCard
          label="Available balance"
          value={
            wallet.available_micros != null
              ? formatUsd(wallet.available_micros)
              : "—"
          }
          hint={`API key ${wallet.principal}`}
        />
        <StatCard
          label="Payer account"
          value={wallet.payer_account ?? "—"}
          hint="Mint account debited for this key's inference"
        />
      </div>
      {operator && <WalletLookup principal={wallet.principal} />}
      <SectionHeader heading="Add funds" />
      <TopUpForm wallet={wallet} submitting={submitting} result={result} />
      {operator && (
        <>
          <SectionHeader heading="Operator credit" />
          {wallet.operator_credit_available ? (
            <CreditForm
              wallet={wallet}
              submitting={submitting}
              result={result}
              idempotencyKey={idempotencyKey}
            />
          ) : (
            <InlineNotice>
              To grant credit without a card payment, set
              RELAY_WALLET_TREASURY_ACCOUNT on the gateway to a funded Mint
              treasury account.
            </InlineNotice>
          )}
        </>
      )}
    </SectionLayout>
  );
}
