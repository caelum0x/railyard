//! The Character Lock Passport business logic: create → edit → lock → version →
//! resolve → generate-AS-passport → provenance. Handlers stay thin; every
//! invariant lives here.

use std::sync::Arc;

use chrono::Utc;

use passport_core::{
    AssetsPatch, CreateOutcome, CreatePassport, MintMeter, MintMeterRequest, Passport,
    PassportAssets, PassportError, PassportFilter, PassportStore, PassportVersion, PassportView,
    RelayConditioningSink, RelayGenerationRequest, VersionStatus,
};
use passport_provenance::{GenerationRecord, ProvenanceLog};
use passport_resolve::{ConditioningPayload, Modality, ProviderRegistry};

/// Result of a create call, carrying whether it was a fresh insert or a replay.
pub struct CreateResult {
    pub view: PassportView,
    pub created: bool,
}

/// Dependencies are injected as trait objects; tests use in-memory/fake impls.
#[derive(Clone)]
pub struct PassportService {
    store: Arc<dyn PassportStore>,
    provenance: Arc<dyn ProvenanceLog>,
    relay: Arc<dyn RelayConditioningSink>,
    mint: Arc<dyn MintMeter>,
    registry: Arc<ProviderRegistry>,
}

impl PassportService {
    pub fn new(
        store: Arc<dyn PassportStore>,
        provenance: Arc<dyn ProvenanceLog>,
        relay: Arc<dyn RelayConditioningSink>,
        mint: Arc<dyn MintMeter>,
    ) -> Self {
        Self {
            store,
            provenance,
            relay,
            mint,
            registry: Arc::new(ProviderRegistry::default()),
        }
    }

    // ---- lifecycle -------------------------------------------------------

    /// Create a draft passport at version 1. Idempotent on `idempotency_key`.
    pub async fn create(&self, request: CreatePassport) -> Result<CreateResult, PassportError> {
        request.validate()?;
        let now = Utc::now();
        let id = passport_core::new_passport_id();

        let assets = match &request.assets {
            Some(patch) => PassportAssets::empty(request.kind).merged(patch)?,
            None => PassportAssets::empty(request.kind),
        };
        let passport = Passport {
            id: id.clone(),
            kind: request.kind,
            owner: request.owner.clone(),
            status: VersionStatus::Draft,
            version: 1,
            created_at: now,
            updated_at: now,
        };
        let head = PassportVersion {
            passport_id: id,
            version: 1,
            status: VersionStatus::Draft,
            assets,
            rights: request.rights.clone().unwrap_or_default(),
            content_digest: None,
            created_at: now,
            updated_at: now,
            locked_at: None,
        };

        let CreateOutcome {
            passport,
            head,
            created,
        } = self
            .store
            .create(&request.idempotency_key, &passport, &head)
            .await?;
        Ok(CreateResult {
            view: PassportView { passport, head },
            created,
        })
    }

    pub async fn get(&self, id: &str) -> Result<PassportView, PassportError> {
        let passport = self.require_passport(id).await?;
        let head = self
            .store
            .head_version(id)
            .await?
            .ok_or(PassportError::VersionNotFound)?;
        Ok(PassportView { passport, head })
    }

    pub async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<PassportVersion, PassportError> {
        self.require_passport(id).await?;
        self.store
            .get_version(id, version)
            .await?
            .ok_or(PassportError::VersionNotFound)
    }

    pub async fn list(&self, filter: &PassportFilter) -> Result<Vec<Passport>, PassportError> {
        self.store.list_passports(filter).await
    }

    /// Apply a typed patch to the head DRAFT version. A locked head is immutable.
    pub async fn update_draft(
        &self,
        id: &str,
        patch: &AssetsPatch,
    ) -> Result<PassportView, PassportError> {
        let mut passport = self.require_passport(id).await?;
        let mut head = self
            .store
            .head_version(id)
            .await?
            .ok_or(PassportError::VersionNotFound)?;
        if head.is_locked() {
            return Err(PassportError::VersionLocked);
        }
        if patch.kind() != passport.kind {
            return Err(PassportError::KindMismatch(format!(
                "patch is {} but passport is {}",
                patch.kind().as_str(),
                passport.kind.as_str()
            )));
        }
        let now = Utc::now();
        head.assets = head.assets.merged(patch)?;
        head.updated_at = now;
        passport.updated_at = now;
        self.store.upsert(&passport, &head).await?;
        Ok(PassportView { passport, head })
    }

    /// Lock the head version: validate rights, content-address it, freeze forever.
    /// Idempotent — locking a locked head returns it unchanged.
    pub async fn lock(&self, id: &str) -> Result<PassportView, PassportError> {
        let mut passport = self.require_passport(id).await?;
        let head = self
            .store
            .head_version(id)
            .await?
            .ok_or(PassportError::VersionNotFound)?;
        if head.is_locked() {
            return Ok(PassportView { passport, head });
        }
        let now = Utc::now();
        let locked = head.locked(now)?; // validates rights + stamps digest
        passport.status = VersionStatus::Locked;
        passport.updated_at = now;
        self.store.upsert(&passport, &locked).await?;
        Ok(PassportView {
            passport,
            head: locked,
        })
    }

    /// Branch a new DRAFT from the LOCKED head (VISION: fork from a locked one).
    pub async fn new_version(&self, id: &str) -> Result<PassportView, PassportError> {
        let mut passport = self.require_passport(id).await?;
        let head = self
            .store
            .head_version(id)
            .await?
            .ok_or(PassportError::VersionNotFound)?;
        if !head.is_locked() {
            return Err(PassportError::VersionNotLocked);
        }
        let now = Utc::now();
        let next_version = passport.version + 1;
        let draft = PassportVersion {
            passport_id: passport.id.clone(),
            version: next_version,
            status: VersionStatus::Draft,
            assets: head.assets.clone(),
            rights: head.rights.clone(),
            content_digest: None,
            created_at: now,
            updated_at: now,
            locked_at: None,
        };
        passport.version = next_version;
        passport.status = VersionStatus::Draft;
        passport.updated_at = now;
        self.store.upsert(&passport, &draft).await?;
        Ok(PassportView {
            passport,
            head: draft,
        })
    }

    // ---- resolve + generate ---------------------------------------------

    /// Resolve a locked version into provider conditioning (no generation).
    pub async fn resolve(
        &self,
        id: &str,
        provider: &str,
        modality: Modality,
        version: Option<i64>,
    ) -> Result<ConditioningPayload, PassportError> {
        let target = self.locked_target(id, version).await?;
        self.registry.resolve(&target, provider, modality)
    }

    /// Generate AS this passport: resolve conditioning → Relay generation →
    /// record provenance (with a frozen rights snapshot) → best-effort Mint meter.
    pub async fn generate(
        &self,
        id: &str,
        provider: &str,
        modality: Modality,
        version: Option<i64>,
    ) -> Result<GenerationRecord, PassportError> {
        let target = self.locked_target(id, version).await?;
        let payload = self.registry.resolve(&target, provider, modality)?;

        let request = RelayGenerationRequest {
            passport_id: target.passport_id.clone(),
            version: target.version,
            provider: provider.to_string(),
            modality: modality.as_str().to_string(),
            payload: serde_json::to_value(&payload)
                .map_err(|e| PassportError::Relay(format!("encoding conditioning: {e}")))?,
        };
        let generated = self.relay.generate(&request).await?;

        let base = GenerationRecord::new(
            &target,
            provider,
            modality.as_str(),
            generated.output_ref,
            generated.asset_hash,
            Utc::now(),
        );

        // Metering is best-effort: a Mint hiccup must not lose provenance of a
        // successful generation (ADR-0002, success-only, keyed by generation id).
        let receipt = self
            .mint
            .meter(&MintMeterRequest {
                reference: base.id.clone(),
                passport_id: base.passport_id.clone(),
                version: base.version,
                provider: provider.to_string(),
                modality: modality.as_str().to_string(),
            })
            .await
            .ok()
            .and_then(|r| r.receipt_ref);

        let record = base.with_receipt(receipt);
        self.provenance.record(&record).await?;
        Ok(record)
    }

    pub async fn generations(&self, id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        self.require_passport(id).await?;
        self.provenance.list(id).await
    }

    pub async fn ready(&self) -> Result<(), PassportError> {
        self.store.ready().await
    }

    // ---- helpers ---------------------------------------------------------

    async fn require_passport(&self, id: &str) -> Result<Passport, PassportError> {
        self.store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)
    }

    /// Fetch the target locked version: an explicit version must itself be locked;
    /// otherwise the latest locked version is used. Fail-closed if none locked.
    async fn locked_target(
        &self,
        id: &str,
        version: Option<i64>,
    ) -> Result<PassportVersion, PassportError> {
        self.require_passport(id).await?;
        match version {
            Some(v) => {
                let target = self
                    .store
                    .get_version(id, v)
                    .await?
                    .ok_or(PassportError::VersionNotFound)?;
                if !target.is_locked() {
                    return Err(PassportError::VersionNotLocked);
                }
                Ok(target)
            }
            None => self
                .store
                .latest_locked_version(id)
                .await?
                .ok_or(PassportError::VersionNotLocked),
        }
    }
}
