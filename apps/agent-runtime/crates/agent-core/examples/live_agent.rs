//! Live Relay + Mint agent executable driven entirely by environment config.

use std::sync::Arc;

use agent_core::aitp_client::{AmountMicros, Ed25519Signer, SettlementEvidenceVerifier};
use agent_core::relay_provider::{RelayConfig, RelayModel};
use agent_core::{RuntimeAgent, RuntimeConfig};
use anyhow::{bail, Context};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let relay_url = require("RELAY_URL")?;
    let relay_api_key = require("RELAY_API_KEY")?;
    let relay_model = require("RELAY_MODEL")?;
    let payer = require("MINT_PAYER_ACCOUNT")?;
    let payee = require("AITP_PAYEE")?;
    let key_id = require("MINT_PAYER_KEY_ID")?;
    let signing_seed = signing_seed()?;
    let verifier_key_id = require("MINT_VERIFIER_KEY_ID")?;
    let verifier_public_key = key_bytes("MINT_VERIFIER_PUBLIC_KEY")?;
    let max_amount = require("AITP_MAX_AMOUNT_MICROS")?
        .parse::<i64>()
        .context("AITP_MAX_AMOUNT_MICROS must be an integer")?;
    if max_amount <= 0 {
        bail!("AITP_MAX_AMOUNT_MICROS must be positive");
    }

    let signer = Arc::new(Ed25519Signer::from_seed(key_id.clone(), &signing_seed));
    let evidence_verifier =
        SettlementEvidenceVerifier::from_public_key_bytes(verifier_key_id, verifier_public_key)
            .context("building pinned Mint settlement verifier")?;
    eprintln!(
        "Mint payer key registration: payer={payer} pubkey_id={key_id} public_key_hex={}",
        hex::encode(signer.verifying_key_bytes())
    );
    let relay_config = RelayConfig::model_name(relay_url, relay_api_key, &relay_model);
    let relay = RelayModel::connect_paid(
        &relay_config,
        payer,
        payee,
        AmountMicros(max_amount),
        signer,
        evidence_verifier,
    )
    .context("building policy-gated AITP Relay model")?;
    let agent = RuntimeAgent::new(
        relay,
        &RuntimeConfig {
            preamble: optional("AGENT_PREAMBLE"),
            temperature: parse_optional("AGENT_TEMPERATURE")?,
            max_tokens: parse_optional("AGENT_MAX_TOKENS")?,
        },
    );

    let prompt = optional("AGENT_PROMPT")
        .unwrap_or_else(|| "Introduce yourself in one concise sentence.".to_string());
    let reply = agent.run(&prompt).await.context("paid agent run failed")?;
    println!("{reply}");
    Ok(())
}

fn require(name: &str) -> anyhow::Result<String> {
    optional(name).ok_or_else(|| anyhow::anyhow!("{name} must be set"))
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_optional<T>(name: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    optional(name)
        .map(|value| {
            value
                .parse()
                .map_err(|error| anyhow::anyhow!("{name} is invalid: {error}"))
        })
        .transpose()
}

fn signing_seed() -> anyhow::Result<[u8; 32]> {
    key_bytes("MINT_PAYER_SIGNING_KEY")
}

fn key_bytes(name: &str) -> anyhow::Result<[u8; 32]> {
    let encoded = require(name)?;
    let decoded = hex::decode(encoded).with_context(|| format!("{name} must be hex"))?;
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must decode to 32 bytes"))
}
