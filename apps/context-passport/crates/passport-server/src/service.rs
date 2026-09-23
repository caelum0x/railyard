//! The Identity Lock business logic: create → edit → lock → version →
//! resolve → generate-AS-passport → provenance. Handlers stay thin; every
//! invariant lives here.

use std::sync::Arc;

use chrono::Utc;

use passport_core::{
    normalize_provider, validate_artifact_ref, ArtifactKind, ArtifactStatus, ArtifactStore,
    TrainedArtifact, AssetsPatch, CreateOutcome, CreatePassport, Passport, PassportAssets, PassportError,
    PassportFilter, PassportStore, PassportVersion, PassportView, RelayConditioningSink,
    RelayGenerationRequest, VersionStatus,
};
use passport_provenance::{to_c2pa_manifest, GenerationRecord, ProvenanceLog};
use passport_resolve::{ConditioningPayload, Modality, ProviderRegistry};

use crate::auth::Principal;
use crate::receipts::ReceiptVerifier;

/// Longest generation instruction accepted (characters).
pub const MAX_PROMPT_CHARS: usize = 8_000;

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
    registry: Arc<ProviderRegistry>,
    /// Trusted Mint receipt keys. Required for `attest`; when present, Relay
    /// evidence on `generate` is verified too.
    receipt_verifier: Option<Arc<ReceiptVerifier>>,
    /// Provider-minted trained artifacts (voice clones, avatars, LoRAs).
    artifacts: Option<Arc<dyn ArtifactStore>>,
}

/// Register a provider artifact minted from a locked version.
pub struct RegisterArtifact {
    pub provider: String,
    pub kind: ArtifactKind,
    /// Explicit locked version; defaults to the latest locked version.
    pub version: Option<i64>,
    pub artifact_ref: Option<String>,
    pub provider_model: Option<String>,
    /// `pending` (training submitted) or `ready` (usable now). Defaults to
    /// `ready` when an `artifact_ref` is given, `pending` otherwise.
    pub status: Option<ArtifactStatus>,
}

/// Complete or fail a pending artifact (or update a ready one's ref).
pub struct UpdateArtifact {
    pub status: ArtifactStatus,
    pub artifact_ref: Option<String>,
    pub provider_model: Option<String>,
    pub failure_reason: Option<String>,
}

impl PassportService {
    pub fn new(
        store: Arc<dyn PassportStore>,
        provenance: Arc<dyn ProvenanceLog>,
        relay: Arc<dyn RelayConditioningSink>,
    ) -> Self {
        Self {
            store,
            provenance,
            relay,
            registry: Arc::new(ProviderRegistry::default()),
            receipt_verifier: None,
            artifacts: None,
        }
    }

    /// Persist trained artifacts in `store` (returns a new service). Without
    /// it the artifact routes answer `503 storage_unavailable`.
    pub fn with_artifact_store(mut self, store: Arc<dyn ArtifactStore>) -> Self {
        self.artifacts = Some(store);
        self
    }

    /// Verify Mint receipt signatures with `verifier` (returns a new service).
    pub fn with_receipt_verifier(mut self, verifier: Arc<ReceiptVerifier>) -> Self {
        self.receipt_verifier = Some(verifier);
        self
    }

    // ---- access control --------------------------------------------------

    /// Fail unless `principal` may act on passport `id`. A tenant key only sees
    /// its own passports; another owner's passport is reported as not found so
    /// ids cannot be probed.
    pub async fn ensure_access(&self, id: &str, principal: &Principal) -> Result<(), PassportError> {
        let passport = self.require_passport(id).await?;
        if principal.may_access(&passport.owner) {
            Ok(())
        } else {
            Err(PassportError::NotFound)
        }
    }

    /// Every version of a passport, oldest first (the full edit/lock history).
    pub async fn versions(&self, id: &str) -> Result<Vec<PassportVersion>, PassportError> {
        let passport = self.require_passport(id).await?;
        let mut out = Vec::with_capacity(passport.version.max(0) as usize);
        for v in 1..=passport.version {
            if let Some(version) = self.store.get_version(id, v).await? {
                out.push(version);
            }
        }
        Ok(out)
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
            archived_at: None,
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

    /// Archive a passport: it stays readable (versions, provenance,
    /// manifests) but refuses edits, locks, new versions, generation and
    /// attestation until unarchived. Idempotent.
    pub async fn archive(&self, id: &str) -> Result<PassportView, PassportError> {
        let passport = self.require_passport(id).await?;
        if passport.is_archived() {
            return self.get(id).await;
        }
        let now = Utc::now();
        self.store
            .set_archived(id, Some(now), now)
            .await?
            .ok_or(PassportError::NotFound)?;
        self.get(id).await
    }

    /// Restore an archived passport to normal use. Idempotent.
    pub async fn unarchive(&self, id: &str) -> Result<PassportView, PassportError> {
        let passport = self.require_passport(id).await?;
        if !passport.is_archived() {
            return self.get(id).await;
        }
        self.store
            .set_archived(id, None, Utc::now())
            .await?
            .ok_or(PassportError::NotFound)?;
        self.get(id).await
    }

    /// Apply a typed patch to the head DRAFT version. A locked head is immutable.
    pub async fn update_draft(
        &self,
        id: &str,
        patch: &AssetsPatch,
    ) -> Result<PassportView, PassportError> {
        let mut passport = self.require_active(id).await?;
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
        let mut passport = self.require_active(id).await?;
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
        let mut passport = self.require_active(id).await?;
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
        let artifacts = self.ready_artifacts(&target).await?;
        self.registry
            .resolve_with_artifacts(&target, provider, modality, &artifacts)
    }

    /// Generate AS this passport: resolve conditioning → Relay generation →
    /// require Relay's correlated signed settlement → record provenance with a
    /// frozen rights snapshot and the exact payment evidence.
    pub async fn generate(
        &self,
        id: &str,
        provider: &str,
        modality: Modality,
        version: Option<i64>,
        prompt: Option<String>,
    ) -> Result<GenerationRecord, PassportError> {
        let prompt = prompt
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        if prompt
            .as_ref()
            .is_some_and(|p| p.chars().count() > MAX_PROMPT_CHARS)
        {
            return Err(PassportError::Invalid(format!(
                "prompt must be at most {MAX_PROMPT_CHARS} characters"
            )));
        }
        let principal = self.require_active(id).await?.owner;
        let target = self.locked_target(id, version).await?;
        let artifacts = self.ready_artifacts(&target).await?;
        let payload = self
            .registry
            .resolve_with_artifacts(&target, provider, modality, &artifacts)?;

        let request = RelayGenerationRequest {
            principal: principal.clone(),
            passport_id: target.passport_id.clone(),
            version: target.version,
            provider: provider.to_string(),
            modality: modality.as_str().to_string(),
            payload: serde_json::to_value(&payload)
                .map_err(|e| PassportError::Relay(format!("encoding conditioning: {e}")))?,
            prompt: prompt.clone(),
        };
        let generated = self.relay.generate(&request).await?;
        let evidence = generated.payment_evidence.clone().ok_or_else(|| {
            PassportError::Relay("conditioned generation missing signed payment evidence".into())
        })?;
        // Apply the same terminal-settlement invariants the attest path enforces,
        // so the service layer stays fail-closed regardless of which
        // `RelayConditioningSink` implementation is injected.
        check_settlement_evidence(&principal, &evidence).map_err(PassportError::Relay)?;
        if let Some(verifier) = &self.receipt_verifier {
            verifier.verify(&evidence).map_err(PassportError::Relay)?;
        }

        // Derive the provenance id from Relay's correlated request id (the natural
        // idempotency key). A retried settled generation therefore reuses the
        // already-recorded provenance instead of double-writing a second row with
        // a fresh random id + timestamp.
        let record_id = format!("gen_{}", evidence.request_id.trim());
        if let Some(existing) = self.provenance.get(&record_id).await? {
            return Ok(existing);
        }

        let base = GenerationRecord::new_with_id(
            record_id,
            &target,
            provider,
            modality.as_str(),
            generated.output_ref,
            generated.asset_hash,
            Utc::now(),
        );

        let record = base
            .with_payment_evidence(evidence)
            .with_output(prompt, generated.output_text);
        self.provenance.record(&record).await?;
        Ok(record)
    }

    /// Attest a generation executed outside Identity Lock itself.
    ///
    /// This is intentionally not a blind metadata append: the server resolves
    /// the immutable locked version again, requires owner/request-correlated
    /// signed settlement evidence, and derives the record id + timestamp from
    /// durable Relay facts so retries are byte-identical.
    pub async fn attest(
        &self,
        id: &str,
        provider: &str,
        modality: Modality,
        version: Option<i64>,
        output_ref: String,
        asset_hash: Option<String>,
        evidence: passport_core::PaymentEvidence,
        settled_at_ms: i64,
    ) -> Result<GenerationRecord, PassportError> {
        self.require_active(id).await?;
        let target = self.locked_target(id, version).await?;
        // Prove this exact locked version has a real conditioning path for the
        // selected provider/modality. An unsupported combination cannot be
        // stamped as identity-conditioned after the fact.
        let artifacts = self.ready_artifacts(&target).await?;
        self.registry
            .resolve_with_artifacts(&target, provider, modality, &artifacts)?;
        let owner = self.require_passport(id).await?.owner;
        validate_external_attestation(&owner, &output_ref, &evidence)?;
        // The signature must be a real Mint Ed25519 signature over the receipt
        // hash. Without trusted keys, attestation fails closed.
        let verifier = self.receipt_verifier.as_ref().ok_or_else(|| {
            PassportError::Mint(
                "receipt verification is not configured (set MINT_RECEIPT_PUBLIC_KEYS or MINT_BASE_URL)"
                    .into(),
            )
        })?;
        verifier.verify(&evidence).map_err(PassportError::Invalid)?;
        // One settled receipt backs exactly one generation of this passport: a
        // genuine receipt cannot be replayed under a fresh request id.
        let record_id_for_request = format!("gen_ext_{}", evidence.request_id);
        if self.provenance.list(id).await?.iter().any(|r| {
            r.id != record_id_for_request
                && r.payment_evidence.as_ref().map(|e| &e.receipt.receipt_hash)
                    == Some(&evidence.receipt.receipt_hash)
        }) {
            return Err(PassportError::IdempotencyConflict);
        }
        let created_at = chrono::DateTime::from_timestamp_millis(settled_at_ms)
            .ok_or_else(|| PassportError::Invalid("settled_at_ms is invalid".into()))?;
        let record_id = format!("gen_ext_{}", evidence.request_id);
        let record = GenerationRecord::new_with_id(
            record_id,
            &target,
            provider,
            modality.as_str(),
            output_ref,
            asset_hash,
            created_at,
        )
        .with_payment_evidence(evidence);
        self.provenance.record(&record).await?;
        Ok(record)
    }

    pub async fn generations(&self, id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        self.require_passport(id).await?;
        self.provenance.list(id).await
    }

    /// Render the C2PA-aligned Content Credential manifest for a single recorded
    /// generation. Fail-closed: the generation must exist AND belong to this
    /// passport, so a caller cannot read another passport's provenance by id.
    pub async fn generation_manifest(
        &self,
        id: &str,
        generation_id: &str,
    ) -> Result<serde_json::Value, PassportError> {
        self.require_passport(id).await?;
        let record = self
            .provenance
            .get(generation_id)
            .await?
            .filter(|record| record.passport_id == id)
            .ok_or(PassportError::NotFound)?;
        Ok(to_c2pa_manifest(&record))
    }

    // ---- trained artifacts ----------------------------------------------

    fn artifact_store(&self) -> Result<&Arc<dyn ArtifactStore>, PassportError> {
        self.artifacts.as_ref().ok_or_else(|| {
            PassportError::Storage("trained-artifact storage is not configured".into())
        })
    }

    /// Ready artifacts for exactly this locked version (empty without a store).
    async fn ready_artifacts(
        &self,
        target: &PassportVersion,
    ) -> Result<Vec<TrainedArtifact>, PassportError> {
        let Some(store) = &self.artifacts else {
            return Ok(Vec::new());
        };
        Ok(store
            .list_artifacts(&target.passport_id)
            .await?
            .into_iter()
            .filter(|a| a.version == target.version && a.status == ArtifactStatus::Ready)
            .collect())
    }

    /// Register a provider artifact for a LOCKED version. Artifacts are derived
    /// data, so this never touches the version itself; a newer registration for
    /// the same `(version, provider, kind)` supersedes the previous one.
    pub async fn register_artifact(
        &self,
        id: &str,
        request: RegisterArtifact,
    ) -> Result<TrainedArtifact, PassportError> {
        let store = self.artifact_store()?;
        self.require_active(id).await?;
        let target = self.locked_target(id, request.version).await?;
        let provider = normalize_provider(&request.provider)?;
        let artifact_ref = validate_artifact_ref(request.artifact_ref.as_deref())?;
        let provider_model = validate_artifact_ref(request.provider_model.as_deref())?;
        let status = request.status.unwrap_or(if artifact_ref.is_some() {
            ArtifactStatus::Ready
        } else {
            ArtifactStatus::Pending
        });
        match status {
            ArtifactStatus::Ready if artifact_ref.is_none() => {
                return Err(PassportError::Invalid(
                    "a ready artifact needs the provider's artifact_ref".into(),
                ))
            }
            ArtifactStatus::Failed | ArtifactStatus::Revoked => {
                return Err(PassportError::Invalid(
                    "new artifacts start as pending or ready".into(),
                ))
            }
            _ => {}
        }
        let now = Utc::now();
        let artifact = TrainedArtifact {
            id: passport_core::new_artifact_id(),
            passport_id: target.passport_id.clone(),
            version: target.version,
            source_digest: target
                .content_digest
                .clone()
                .or_else(|| Some(target.compute_digest())),
            provider,
            kind: request.kind,
            artifact_ref,
            provider_model,
            status,
            failure_reason: None,
            created_at: now,
            updated_at: now,
        };
        store.insert_artifact(&artifact).await?;
        Ok(artifact)
    }

    pub async fn artifacts(&self, id: &str) -> Result<Vec<TrainedArtifact>, PassportError> {
        let store = self.artifact_store()?;
        self.require_passport(id).await?;
        store.list_artifacts(id).await
    }

    pub async fn artifact(
        &self,
        id: &str,
        artifact_id: &str,
    ) -> Result<TrainedArtifact, PassportError> {
        let store = self.artifact_store()?;
        self.require_passport(id).await?;
        store
            .get_artifact(artifact_id)
            .await?
            .filter(|a| a.passport_id == id)
            .ok_or(PassportError::ArtifactNotFound)
    }

    /// Move an artifact through its lifecycle:
    /// pending -> ready | failed; ready -> ready (ref rotation). Revocation is
    /// [`Self::revoke_artifact`]; revoked and failed artifacts are terminal.
    pub async fn update_artifact(
        &self,
        id: &str,
        artifact_id: &str,
        update: UpdateArtifact,
    ) -> Result<TrainedArtifact, PassportError> {
        let store = self.artifact_store()?;
        self.require_active(id).await?;
        let current = self.artifact(id, artifact_id).await?;
        let allowed = matches!(
            (current.status, update.status),
            (ArtifactStatus::Pending, ArtifactStatus::Ready)
                | (ArtifactStatus::Pending, ArtifactStatus::Failed)
                | (ArtifactStatus::Ready, ArtifactStatus::Ready)
        );
        if !allowed {
            return Err(PassportError::ArtifactState(format!(
                "cannot move a {} artifact to {}",
                current.status.as_str(),
                update.status.as_str()
            )));
        }
        let mut next = current.clone();
        if let Some(reference) = validate_artifact_ref(update.artifact_ref.as_deref())? {
            next.artifact_ref = Some(reference);
        }
        if let Some(model) = validate_artifact_ref(update.provider_model.as_deref())? {
            next.provider_model = Some(model);
        }
        next.status = update.status;
        next.failure_reason = match update.status {
            ArtifactStatus::Failed => {
                let reason = update
                    .failure_reason
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "provider reported a failure".to_string());
                if reason.len() > passport_core::artifact::MAX_FAILURE_REASON_LEN {
                    return Err(PassportError::Invalid(
                        "failure_reason must be at most 1000 characters".into(),
                    ));
                }
                Some(reason)
            }
            _ => None,
        };
        if next.status == ArtifactStatus::Ready && next.artifact_ref.is_none() {
            return Err(PassportError::Invalid(
                "a ready artifact needs the provider's artifact_ref".into(),
            ));
        }
        next.updated_at = Utc::now();
        store
            .update_artifact(&next)
            .await?
            .ok_or(PassportError::ArtifactNotFound)
    }

    /// Revoke an artifact (consent withdrawn, clone deleted at the provider).
    /// Allowed on archived passports so consent can always be withdrawn.
    /// Idempotent. The record is kept for provenance.
    pub async fn revoke_artifact(
        &self,
        id: &str,
        artifact_id: &str,
        reason: Option<String>,
    ) -> Result<TrainedArtifact, PassportError> {
        let store = self.artifact_store()?;
        let current = self.artifact(id, artifact_id).await?;
        if current.status == ArtifactStatus::Revoked {
            return Ok(current);
        }
        let mut next = current;
        next.status = ArtifactStatus::Revoked;
        next.failure_reason = reason
            .map(|r| r.trim().chars().take(passport_core::artifact::MAX_FAILURE_REASON_LEN).collect::<String>())
            .filter(|r| !r.is_empty())
            .or_else(|| Some("revoked".to_string()));
        next.updated_at = Utc::now();
        store
            .update_artifact(&next)
            .await?
            .ok_or(PassportError::ArtifactNotFound)
    }

    pub async fn ready(&self) -> Result<(), PassportError> {
        self.store.ready().await
    }

    // ---- helpers ---------------------------------------------------------

    /// Like `require_passport`, but refuses an archived passport.
    async fn require_active(&self, id: &str) -> Result<Passport, PassportError> {
        let passport = self.require_passport(id).await?;
        if passport.is_archived() {
            return Err(PassportError::Archived);
        }
        Ok(passport)
    }

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

/// The terminal-settlement invariants shared by both the in-service `generate`
/// path (evidence from Relay) and the `attest` path (evidence from a caller):
/// the request id is a bounded id-safe token, the payer owns the passport, and
/// the receipt is a signed, non-negative, terminal settlement. Returns the
/// violation message so each caller can wrap it in its own error variant.
fn check_settlement_evidence(
    owner: &str,
    evidence: &passport_core::PaymentEvidence,
) -> Result<(), String> {
    let request_id = evidence.request_id.trim();
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("payment evidence request_id is invalid".into());
    }
    if evidence.payer_account != owner {
        return Err("payment evidence payer does not own this passport".into());
    }
    let receipt = &evidence.receipt;
    if !matches!(receipt.operation.as_str(), "hold.capture" | "transfer")
        || receipt.operation_hash.is_empty()
        || receipt.receipt_hash.is_empty()
        || receipt.signature.is_empty()
        || receipt.amount_micros < 0
    {
        return Err("payment evidence is not a signed terminal settlement".into());
    }
    Ok(())
}

fn validate_external_attestation(
    owner: &str,
    output_ref: &str,
    evidence: &passport_core::PaymentEvidence,
) -> Result<(), PassportError> {
    if output_ref.trim().is_empty() || output_ref.len() > 4_096 {
        return Err(PassportError::Invalid(
            "external generation requires a bounded output_ref".into(),
        ));
    }
    check_settlement_evidence(owner, evidence).map_err(PassportError::Invalid)
}
