//! Live Relay + Mint agent executable driven entirely by environment config.

use std::sync::Arc;

use agent_core::aitp_client::{AmountMicros, Ed25519Signer, MintPaymentGateway};
use agent_core::relay_provider::{RelayConfig, RelayModel};
use agent_core::{PricedRoute, RuntimeAgent, RuntimeConfig};
use anyhow::{bail, Context};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if optional("AGENT_DIRECT_SETTLEMENT_ACK").as_deref() != Some("1") {
        bail!(
            "set AGENT_DIRECT_SETTLEMENT_ACK=1 after confirming this Relay route does not also \
             apply Relay billing; otherwise the request could be charged twice"
        );
    }
    let relay_url = require("RELAY_URL")?;
    let relay_api_key = require("RELAY_API_KEY")?;
    let relay_model = require("RELAY_MODEL")?;
    let mint_url = require("MINT_SERVER_URL")?;
    let mint_api_key = require("MINT_API_KEY")?;
    let payer = require("MINT_PAYER_ACCOUNT")?;
    let payee = require("AITP_PAYEE")?;
    let key_id = require("MINT_PAYER_KEY_ID")?;
    let signing_seed = signing_seed()?;
    let price = require("AITP_PRICE_MICROS")?
        .parse::<i64>()
        .context("AITP_PRICE_MICROS must be an integer")?;
    if price <= 0 {
        bail!("AITP_PRICE_MICROS must be positive");
    }

    let signer = Arc::new(Ed25519Signer::from_seed(key_id.clone(), &signing_seed));
    eprintln!(
        "Mint payer key registration: payer={payer} pubkey_id={key_id} public_key_hex={}",
        hex::encode(signer.verifying_key_bytes())
    );
    let payment = Arc::new(
        MintPaymentGateway::with_reqwest(mint_url, payer, mint_api_key, signer)
            .context("building Mint payment gateway")?,
    );
    let relay = RelayModel::connect(&RelayConfig::model_name(
        relay_url,
        relay_api_key,
        &relay_model,
    ))
    .context("building Relay model")?;
    let resource = format!("POST /openai/v1/chat/completions#{relay_model}");
    let agent = RuntimeAgent::paid(
        relay,
        payment,
        PricedRoute::new(payee, AmountMicros(price), resource),
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
    let encoded = require("MINT_PAYER_SIGNING_KEY")?;
    let decoded = hex::decode(encoded).context("MINT_PAYER_SIGNING_KEY must be hex")?;
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("MINT_PAYER_SIGNING_KEY must decode to 32 bytes"))
}
