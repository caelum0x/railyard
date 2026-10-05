import { BillingActions } from "../../../components/BillingActions.tsx";
import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";
import { formatCents, PLAN } from "../../../lib/plans.ts";

export default async function BillingPage({ searchParams }: { searchParams: { error?: string; checkout?: string } }) {
  const { repo, cfg } = deps();
  const s = (await currentSession())!;
  const ent = await repo.getEntitlement(s.org);
  const a = access(ent, new Date());
  return (
    <>
      <h1>Billing</h1>
      {searchParams.error === "arriving" ? (
        <p className="muted">Card payments open soon. Nothing was charged.</p>
      ) : searchParams.error === "already" ? (
        <p className="muted">That is already on your account, so nothing new was charged.</p>
      ) : searchParams.error ? (
        <p className="error">That did not go through. Nothing was charged; try again.</p>
      ) : null}
      {searchParams.checkout && a.state === "no_card" ? <p className="muted">Waiting for the payment confirmation. This page updates when it arrives.</p> : null}
      {!cfg.billingReady && a.state === "no_card" ? (
        <p className="muted">Card payments open soon. Railyard bought with the Scribase add-on works now.</p>
      ) : null}
      <section className="card">
        <BillingActions
          state={a.state}
          trialDaysLeft={a.trialDaysLeft}
          interval={ent?.interval ?? "month"}
          paused={ent?.paused ?? false}
          hasCard={!!ent?.card_verified_at}
          trialUsed={!!ent?.trial_ends_at}
          credit={formatCents(ent?.credit_cents ?? 0)}
          monthPrice={`$${PLAN.monthCents / 100}`}
          yearPrice={`$${PLAN.yearCents / 100}`}
        />
      </section>
    </>
  );
}
