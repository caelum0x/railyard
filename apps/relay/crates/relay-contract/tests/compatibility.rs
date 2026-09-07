use relay_contract::{
    AitpChallenge, ApiErrorEnvelope, DataEnvelope, GatewayHealth, GatewayStatus, Microdollars,
    SettledReplay, SettlementEvidence, Usage,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn fixture<T: DeserializeOwned>(json: &str) -> serde_json::Result<T> {
    serde_json::from_str(json)
}

fn assert_round_trip<T>(json: &str) -> serde_json::Result<()>
where
    T: DeserializeOwned + Serialize,
{
    let expected: serde_json::Value = serde_json::from_str(json)?;
    let typed: T = fixture(json)?;
    assert_eq!(serde_json::to_value(typed)?, expected);
    Ok(())
}

#[test]
fn v1_status_health_and_error_shapes_are_stable() -> Result<(), Box<dyn std::error::Error>> {
    let status_json = include_str!("fixtures/status.json");
    let status: GatewayStatus = fixture(status_json)?;
    assert!(status.is_ready());
    assert_round_trip::<GatewayStatus>(status_json)?;

    let health_json = include_str!("fixtures/health.json");
    let health: GatewayHealth = fixture(health_json)?;
    assert!(health.is_healthy());
    assert_round_trip::<GatewayHealth>(health_json)?;

    let error_json = include_str!("fixtures/api_error.json");
    let error: ApiErrorEnvelope = fixture(error_json)?;
    assert_eq!(error.message(), "requested Relay model was not found");
    assert_round_trip::<ApiErrorEnvelope>(error_json)?;
    Ok(())
}

#[test]
fn v1_payment_and_settlement_shapes_are_stable() -> Result<(), Box<dyn std::error::Error>> {
    let challenge_json = include_str!("fixtures/aitp_challenge.json");
    let challenge: AitpChallenge = fixture(challenge_json)?;
    assert_eq!(challenge.amount_micros, Microdollars::new(7_500));
    assert_eq!(AitpChallenge::decode(&challenge.encode()?), Ok(challenge));
    assert_round_trip::<AitpChallenge>(challenge_json)?;

    let evidence_json = include_str!("fixtures/settlement_evidence.json");
    let evidence: DataEnvelope<SettlementEvidence> = fixture(evidence_json)?;
    assert_eq!(evidence.data.captured_micros(), Microdollars::new(2_500));
    assert_round_trip::<DataEnvelope<SettlementEvidence>>(evidence_json)?;

    let replay_json = include_str!("fixtures/settled_replay.json");
    let replay: SettledReplay = fixture(replay_json)?;
    assert_eq!(
        replay.error.code.as_deref(),
        Some("request_already_settled")
    );
    assert_eq!(replay.data.captured_micros(), Microdollars::new(2_500));
    assert_eq!(
        replay.data.receipt.balance_after_micros,
        Some(Microdollars::new(997_500))
    );
    assert_eq!(
        replay.data.receipt.prev_hash.as_deref(),
        Some("0000000000000000000000000000000000000000000000000000000000000000")
    );
    assert_round_trip::<SettledReplay>(replay_json)?;
    Ok(())
}

#[test]
fn v1_openai_usage_normalizes_exact_decimal_cost() -> Result<(), Box<dyn std::error::Error>> {
    let response: serde_json::Value = fixture(include_str!("fixtures/openai_usage.json"))?;
    let usage = Usage::from_response(&response)?.ok_or("fixture usage missing")?;
    assert_eq!(usage.input_tokens, 1_000);
    assert_eq!(usage.output_tokens, 500);
    assert_eq!(usage.total_tokens, 1_500);
    assert_eq!(usage.provider_cost_micros, Some(Microdollars::new(7_500)));
    Ok(())
}
