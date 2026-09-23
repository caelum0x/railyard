//! Parity with Lago's canonical `examples/agentic-ai-demo`.
//!
//! Two `sum_agg` billable metrics on the `tokens` field with sub-cent per-unit
//! pricing:
//!   * input tokens  @ $0.000002/token
//!   * output tokens @ $0.000008/token
//!
//! 3 AI requests ingest 5,000 input tokens and 1,250 output tokens:
//!   5,000 × 0.000002 = $0.01  +  1,250 × 0.000008 = $0.01  =  **$0.02** (2¢).
//!
//! This exercises exact sub-cent decimal precision end-to-end (aggregation →
//! charge model → fee cents → invoice) and asserts Rago reproduces Lago's
//! published number of exactly 2 cents.

use chrono::Utc;
use rago_billing::charge_to_fee;
use rago_billing::invoices::assemble_invoice;
use rago_core::enums::{
    AggregationType, ChargeModel, InvoiceStatus, InvoiceType, PaymentStatus, SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Event, Invoice, Subscription};
use rago_core::money::Currency;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn token_metric(code: &str) -> BillableMetric {
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: code.into(),
        code: code.into(),
        description: None,
        aggregation_type: AggregationType::SumAgg,
        field_name: Some("tokens".into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    }
}

fn token_charge(amount: &str) -> Charge {
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: None,
        plan_id: None,
        code: "c".into(),
        charge_model: ChargeModel::Standard,
        properties: json!({ "amount": amount }),
        pay_in_advance: false,
        prorated: false,
        invoiceable: true,
        min_amount_cents: 0,
        amount_currency: Some("USD".into()),
        accepts_target_wallet: false,
        invoice_display_name: None,
        parent_id: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    }
}

fn token_events(counts: &[i64]) -> Vec<Event> {
    counts
        .iter()
        .map(|t| Event {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            customer_id: None,
            subscription_id: None,
            transaction_id: Uuid::new_v4().to_string(),
            code: "tokens".into(),
            properties: json!({ "tokens": t }),
            external_customer_id: None,
            external_subscription_id: None,
            timestamp: None,
            precise_total_amount_cents: None,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
            persisted: true,
        })
        .collect()
}

fn subscription() -> Subscription {
    let org = Uuid::new_v4();
    Subscription {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Uuid::new_v4(),
        plan_id: Uuid::new_v4(),
        billing_entity_id: Some(org),
        external_id: "agentic-ai-demo-subscription".into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: rago_core::enums::BillingTime::Anniversary,
        subscription_at: None,
        started_at: None,
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn blank_invoice() -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        customer_id: Some(Uuid::new_v4()),
        number: "INV-AI".into(),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: None,
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

#[test]
fn agentic_ai_demo_totals_exactly_two_cents() {
    let usd = Currency::new("USD");
    let sub = subscription();

    // Input tokens: 1000 + 2500 + 1500 = 5000 @ $0.000002 = $0.01 → 1¢.
    let input_metric = token_metric("agentic-ai-demo-input-tokens");
    let input_charge = token_charge("0.000002");
    let input_events = token_events(&[1000, 2500, 1500]);
    let input_fee =
        charge_to_fee(&input_metric, &input_charge, &sub, &input_events, usd, None).unwrap();

    // Output tokens: 250 + 600 + 400 = 1250 @ $0.000008 = $0.01 → 1¢.
    let output_metric = token_metric("agentic-ai-demo-output-tokens");
    let output_charge = token_charge("0.000008");
    let output_events = token_events(&[250, 600, 400]);
    let output_fee = charge_to_fee(
        &output_metric,
        &output_charge,
        &sub,
        &output_events,
        usd,
        None,
    )
    .unwrap();

    // Exact sub-cent units and per-fee cents.
    assert_eq!(input_fee.units, dec!(5000));
    assert_eq!(output_fee.units, dec!(1250));
    assert_eq!(input_fee.amount_cents, 1); // round($0.010000, 2) * 100
    assert_eq!(output_fee.amount_cents, 1);

    // Invoice: usage total is exactly 2 cents ($0.02).
    let mut invoice = blank_invoice();
    let mut fees = vec![input_fee, output_fee];
    assemble_invoice(&mut invoice, &mut fees, &[], &[]);

    assert_eq!(invoice.fees_amount_cents, 2);
    assert_eq!(invoice.total_amount_cents, 2);
}
