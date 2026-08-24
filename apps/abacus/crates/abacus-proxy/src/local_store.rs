//! File-backed product store for small Abacus deployments.
//!
//! Agent budget policy is loaded from a declarative JSON file at startup. Mint
//! remains the authoritative money ledger; this store only resolves agent
//! policy and appends settled receipt views to a local JSONL audit log.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use abacus_core::error::AbacusError;
use abacus_core::identity::{AgentIdentity, BudgetPolicy, BudgetRepo, FundingPath};
use abacus_core::ids::{AccountId, AgentId, Ait1Address, BudgetId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptStore};
use async_trait::async_trait;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AgentConfigFile {
    agents: Vec<ConfiguredAgent>,
}

#[derive(Debug, Deserialize)]
struct ConfiguredAgent {
    agent_id: AgentId,
    agent_acct: AccountId,
    treasury: AccountId,
    payee: Ait1Address,
    budget: BudgetPolicy,
}

/// A restart-safe policy and receipt store intended for single-instance and
/// development deployments. Multi-instance deployments should replace this
/// port with a transactional shared database implementation.
pub struct LocalStore {
    agents: HashMap<String, ConfiguredAgent>,
    receipts: Mutex<ReceiptLog>,
}

struct ReceiptLog {
    path: PathBuf,
    by_hold: HashMap<String, MintReceipt>,
}

impl LocalStore {
    /// Load configured agents and rebuild the receipt index from the JSONL log.
    pub fn open(
        agent_config_path: impl AsRef<Path>,
        receipt_log_path: impl AsRef<Path>,
    ) -> Result<Self, AbacusError> {
        let config_path = agent_config_path.as_ref();
        let config_bytes = std::fs::read(config_path).map_err(|error| {
            AbacusError::Config(format!(
                "reading agent config {}: {error}",
                config_path.display()
            ))
        })?;
        let config: AgentConfigFile = serde_json::from_slice(&config_bytes).map_err(|error| {
            AbacusError::Config(format!(
                "parsing agent config {}: {error}",
                config_path.display()
            ))
        })?;

        let mut agents = HashMap::with_capacity(config.agents.len());
        for agent in config.agents {
            validate_agent(&agent).map_err(|detail| {
                AbacusError::Config(format!("{}: {detail}", config_path.display()))
            })?;
            let key = agent.agent_id.as_str().to_string();
            if agents.insert(key.clone(), agent).is_some() {
                return Err(AbacusError::Config(format!(
                    "duplicate agent_id {key} in {}",
                    config_path.display()
                )));
            }
        }
        if agents.is_empty() {
            return Err(AbacusError::Config(format!(
                "{} must configure at least one agent",
                config_path.display()
            )));
        }

        let receipt_path = receipt_log_path.as_ref().to_path_buf();
        let by_hold = load_receipts(&receipt_path)?;
        Ok(Self {
            agents,
            receipts: Mutex::new(ReceiptLog {
                path: receipt_path,
                by_hold,
            }),
        })
    }
}

fn validate_agent(agent: &ConfiguredAgent) -> Result<(), String> {
    AgentId::parse(agent.agent_id.as_str()).map_err(|_| "invalid agent_id".to_string())?;
    AccountId::parse(agent.agent_acct.as_str()).map_err(|_| "invalid agent_acct".to_string())?;
    AccountId::parse(agent.treasury.as_str()).map_err(|_| "invalid treasury".to_string())?;
    Ait1Address::parse(agent.payee.as_str()).map_err(|_| "invalid payee".to_string())?;
    BudgetId::parse(agent.budget.budget_id.as_str())
        .map_err(|_| "invalid budget_id".to_string())?;
    if agent
        .budget
        .per_request_max_micros
        .is_some_and(|value| value < 0)
    {
        return Err("per_request_max_micros must be non-negative".to_string());
    }
    match (&agent.budget.funding_path, &agent.budget.allowance_id) {
        (FundingPath::DirectHold, None) => Ok(()),
        (FundingPath::AllowanceCeiling, Some(id)) if !id.trim().is_empty() => Ok(()),
        (FundingPath::DirectHold, Some(_)) => {
            Err("direct_hold must not configure allowance_id".to_string())
        }
        (FundingPath::AllowanceCeiling, _) => {
            Err("allowance_ceiling requires allowance_id".to_string())
        }
    }
}

fn load_receipts(path: &Path) -> Result<HashMap<String, MintReceipt>, AbacusError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => {
            return Err(AbacusError::Storage(format!(
                "opening receipt log {}: {error}",
                path.display()
            )));
        }
    };

    let mut receipts = HashMap::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| {
            AbacusError::Storage(format!(
                "reading receipt log {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let receipt: MintReceipt = serde_json::from_str(&line).map_err(|error| {
            AbacusError::Storage(format!(
                "parsing receipt log {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        receipts.insert(receipt.hold_id.clone(), receipt);
    }
    Ok(receipts)
}

#[async_trait]
impl BudgetRepo for LocalStore {
    async fn resolve(&self, agent: &AgentId, run: &RunId) -> Result<AgentIdentity, AbacusError> {
        let configured = self
            .agents
            .get(agent.as_str())
            .ok_or(AbacusError::AccountNotFound)?;
        Ok(AgentIdentity {
            agent_acct: configured.agent_acct.clone(),
            run_id: run.clone(),
            treasury: configured.treasury.clone(),
            payee: configured.payee.clone(),
            budget: configured.budget.clone(),
        })
    }
}

#[async_trait]
impl ReceiptStore for LocalStore {
    async fn record(&self, receipt: &MintReceipt) -> Result<(), AbacusError> {
        let mut log = self
            .receipts
            .lock()
            .map_err(|_| AbacusError::Storage("receipt log lock poisoned".to_string()))?;
        if let Some(existing) = log.by_hold.get(&receipt.hold_id) {
            return if existing == receipt {
                Ok(())
            } else {
                Err(AbacusError::Storage(format!(
                    "conflicting receipt for hold {}",
                    receipt.hold_id
                )))
            };
        }

        if let Some(parent) = log.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AbacusError::Storage(format!(
                    "creating receipt directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log.path)
            .map_err(|error| {
                AbacusError::Storage(format!(
                    "opening receipt log {}: {error}",
                    log.path.display()
                ))
            })?;
        serde_json::to_writer(&mut file, receipt)
            .map_err(|error| AbacusError::Storage(format!("serializing receipt: {error}")))?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_data())
            .map_err(|error| AbacusError::Storage(format!("persisting receipt: {error}")))?;
        log.by_hold.insert(receipt.hold_id.clone(), receipt.clone());
        Ok(())
    }

    async fn get(&self, hold_id: &str) -> Result<Option<MintReceipt>, AbacusError> {
        let log = self
            .receipts
            .lock()
            .map_err(|_| AbacusError::Storage("receipt log lock poisoned".to_string()))?;
        Ok(log.by_hold.get(hold_id).cloned())
    }
}
