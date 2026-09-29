"use client";
import { useState } from "react";

type State = "no_card" | "trial" | "trial_ended" | "active" | "paused" | "cancelling" | "cancelled";

export function BillingActions(props: {
  state: State;
  trialDaysLeft: number | null;
  interval: "month" | "year";
  paused: boolean;
  credit: string;
  monthPrice: string;
  yearPrice: string;
}) {
  const [interval, setInterval] = useState<"month" | "year">(props.interval);
  const subscribeForm = (
    <form method="post" action="/api/billing/checkout" className="row">
      <input type="hidden" name="kind" value="plan" />
      <input type="hidden" name="interval" value={interval} />
      <div className="switch" role="group" aria-label="Billing period">
        <button type="button" aria-pressed={interval === "month"} onClick={() => setInterval("month")}>Monthly</button>
        <button type="button" aria-pressed={interval === "year"} onClick={() => setInterval("year")}>Yearly</button>
      </div>
      <button className="primary" type="submit">
        Subscribe, {interval === "year" ? `${props.yearPrice} a year` : `${props.monthPrice} a month`}
      </button>
    </form>
  );
  if (props.state === "no_card" || props.state === "cancelled") {
    return (
      <>
        <h2>Start with a $1 card check</h2>
        <p className="muted">The dollar is credited back to your account, then you get the full plan for 14 days.</p>
        <form method="post" action="/api/billing/checkout">
          <input type="hidden" name="kind" value="card_check" />
          <button className="primary" type="submit">Add a card</button>
        </form>
      </>
    );
  }
  return (
    <>
      <h2>
        {props.state === "trial" ? `Trial: ${props.trialDaysLeft} days left` : props.state === "trial_ended" ? "Trial ended" : props.state === "paused" ? "Paused" : props.state === "cancelling" ? "Ends at the next billing date" : "Railyard plan"}
      </h2>
      <p className="muted">Credit on your account: {props.credit}</p>
      {props.state === "trial" || props.state === "trial_ended" ? subscribeForm : null}
      <div className="row" style={{ marginTop: 18 }}>
        <form method="post" action="/api/billing/pause">
          <input type="hidden" name="paused" value={props.paused ? "0" : "1"} />
          <button className="ghost" type="submit" aria-pressed={props.paused}>{props.paused ? "Resume" : "Pause"}</button>
        </form>
        {props.state !== "cancelling" && props.state !== "trial_ended" ? (
          <form method="post" action="/api/billing/cancel">
            <button className="ghost" type="submit">Cancel</button>
          </form>
        ) : null}
      </div>
    </>
  );
}
