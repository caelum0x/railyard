//! The Character Lock Passport business logic: create → edit → lock → version →
//! resolve → provenance. Handlers stay thin; every invariant lives here.

use std::sync::Arc;

use chrono::Utc;

use crate::domain::{
    new_generation_id, new_passport_id, AssetsPatch, CreatePassport, GenerationRecord, Passport,
    PassportAssets, PassportFilter, PassportVersion, PassportView, VersionStatus,
};
use crate::error::PassportError;
use crate::resolve::{resolve, ConditioningPayload, Modality};
use crate::store::PassportStore;

#[derive(Clone)]
pub struct PassportService {
    store: Arc<dyn PassportStore>,
}

/// Result of a create call, carrying whether it was a fresh insert or a replay.
pub struct CreateResult {
    pub view: PassportView,
    pub created: bool,
}

impl PassportService {
    pub fn new(store: Arc<dyn PassportStore>) -> Self {
        Self { store }
    }

    /// Create a draft passport at version 1. Idempotent on `idempotency_key`.
    pub async fn create(&self, request: CreatePassport) -> Result<CreateResult, PassportError> {
        request.validate()?;
        let now = Utc::now();
        let id = new_passport_id();

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
            created_at: now,
            updated_at: now,
            locked_at: None,
        };

        let outcome = self
            .store
            .create(&request.idempotency_key, &passport, &head)
            .await?;
        Ok(CreateResult {
            view: PassportView {
                passport: outcome.passport,
                head: outcome.head,
            },
            created: outcome.created,
        })
    }

    pub async fn get(&self, id: &str) -> Result<PassportView, PassportError> {
        let passport = self
            .store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
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
        // Prove the passport exists so a bad id is `not_found`, not a silent empty.
        self.store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
        self.store
            .get_version(id, version)
            .await?
            .ok_or(PassportError::VersionNotFound)
    }

    pub async fn list(&self, filter: &PassportFilter) -> Result<Vec<Passport>, PassportError> {
        self.store.list_passports(filter).await
    }

    /// Apply a typed patch to the head DRAFT version. A locked head is immutable
    /// and rejected — callers must branch a new version first.
    pub async fn update_draft(
        &self,
        id: &str,
        patch: &AssetsPatch,
    ) -> Result<PassportView, PassportError> {
        let mut passport = self
            .store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
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

    /// Lock the head version, freezing it forever. Idempotent: locking a locked
    /// head returns it unchanged.
    pub async fn lock(&self, id: &str) -> Result<PassportView, PassportError> {
        let mut passport = self
            .store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
        let mut head = self
            .store
            .head_version(id)
            .await?
            .ok_or(PassportError::VersionNotFound)?;

        if head.is_locked() {
            return Ok(PassportView { passport, head });
        }

        let now = Utc::now();
        head.status = VersionStatus::Locked;
        head.locked_at = Some(now);
        head.updated_at = now;
        passport.status = VersionStatus::Locked;
        passport.updated_at = now;

        self.store.upsert(&passport, &head).await?;
        Ok(PassportView { passport, head })
    }

    /// Branch a new DRAFT version from the head. The head must be LOCKED —
    /// VISION requires new versions to fork from a locked one. The new draft
    /// starts as an exact copy of the locked assets.
    pub async fn new_version(&self, id: &str) -> Result<PassportView, PassportError> {
        let mut passport = self
            .store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
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

    /// Resolve the passport's latest locked version into provider conditioning.
    /// An explicit `version` may target an older locked snapshot; it must be
    /// locked or resolution fails closed.
    pub async fn resolve(
        &self,
        id: &str,
        provider: &str,
        modality: Modality,
        version: Option<i64>,
    ) -> Result<ConditioningPayload, PassportError> {
        self.store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;

        let target = match version {
            Some(v) => self
                .store
                .get_version(id, v)
                .await?
                .ok_or(PassportError::VersionNotFound)?,
            None => self
                .store
                .latest_locked_version(id)
                .await?
                .ok_or(PassportError::VersionNotLocked)?,
        };

        resolve(&target, provider, modality)
    }

    /// Record which passport + version conditioned a generated asset. Requires a
    /// locked version so provenance always points at an immutable snapshot.
    pub async fn record_generation(
        &self,
        id: &str,
        version: i64,
        provider: &str,
        modality: Modality,
        output_ref: &str,
    ) -> Result<GenerationRecord, PassportError> {
        self.store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
        let target = self
            .store
            .get_version(id, version)
            .await?
            .ok_or(PassportError::VersionNotFound)?;
        if !target.is_locked() {
            return Err(PassportError::VersionNotLocked);
        }
        if output_ref.trim().is_empty() {
            return Err(PassportError::Invalid("output_ref must not be empty".into()));
        }

        let record = GenerationRecord {
            id: new_generation_id(),
            passport_id: id.to_string(),
            version,
            provider: provider.to_string(),
            modality: modality.as_str().to_string(),
            output_ref: output_ref.to_string(),
            created_at: Utc::now(),
        };
        self.store.record_generation(&record).await?;
        Ok(record)
    }

    pub async fn generations(
        &self,
        id: &str,
    ) -> Result<Vec<GenerationRecord>, PassportError> {
        self.store
            .get_passport(id)
            .await?
            .ok_or(PassportError::NotFound)?;
        self.store.generations(id).await
    }

    pub async fn ready(&self) -> Result<(), PassportError> {
        self.store.ready().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CharacterPatch, PassportKind};
    use crate::store::MemoryStore;

    fn service() -> PassportService {
        PassportService::new(Arc::new(MemoryStore::default()))
    }

    fn create_character() -> CreatePassport {
        CreatePassport {
            idempotency_key: "idem-1".into(),
            kind: PassportKind::Character,
            owner: "menivor".into(),
            assets: Some(AssetsPatch::Character(CharacterPatch {
                face_refs: Some(vec!["face_a".into()]),
                voice_id: Some("eleven-42".into()),
                lora_ref: Some("lora_y".into()),
                style: Some("cinematic".into()),
                ..Default::default()
            })),
        }
    }

    #[tokio::test]
    async fn create_is_idempotent() {
        let svc = service();
        let first = svc.create(create_character()).await.unwrap();
        let replay = svc.create(create_character()).await.unwrap();
        assert!(first.created);
        assert!(!replay.created);
        assert_eq!(first.view.passport.id, replay.view.passport.id);
    }

    #[tokio::test]
    async fn locking_makes_the_version_immutable() {
        let svc = service();
        let created = svc.create(create_character()).await.unwrap();
        let id = created.view.passport.id;

        svc.lock(&id).await.unwrap();

        let patch = AssetsPatch::Character(CharacterPatch {
            style: Some("noir".into()),
            ..Default::default()
        });
        let err = svc.update_draft(&id, &patch).await.unwrap_err();
        assert_eq!(err, PassportError::VersionLocked);
    }

    #[tokio::test]
    async fn versioning_branches_from_a_locked_head() {
        let svc = service();
        let created = svc.create(create_character()).await.unwrap();
        let id = created.view.passport.id;

        // Cannot branch a draft.
        assert_eq!(
            svc.new_version(&id).await.unwrap_err(),
            PassportError::VersionNotLocked
        );

        svc.lock(&id).await.unwrap();
        let branched = svc.new_version(&id).await.unwrap();
        assert_eq!(branched.head.version, 2);
        assert_eq!(branched.head.status, VersionStatus::Draft);
        assert_eq!(branched.passport.status, VersionStatus::Draft);

        // The new draft is editable again.
        let patch = AssetsPatch::Character(CharacterPatch {
            style: Some("noir".into()),
            ..Default::default()
        });
        let edited = svc.update_draft(&id, &patch).await.unwrap();
        let PassportAssets::Character(c) = edited.head.assets else {
            panic!("kind changed");
        };
        assert_eq!(c.style.as_deref(), Some("noir"));
        // Version 1 (locked) is untouched.
        let v1 = svc.get_version(&id, 1).await.unwrap();
        let PassportAssets::Character(c1) = v1.assets else {
            panic!("kind changed");
        };
        assert_eq!(c1.style.as_deref(), Some("cinematic"));
    }

    #[tokio::test]
    async fn resolve_requires_a_locked_version() {
        let svc = service();
        let created = svc.create(create_character()).await.unwrap();
        let id = created.view.passport.id;
        assert_eq!(
            svc.resolve(&id, "runway", Modality::Image, None)
                .await
                .unwrap_err(),
            PassportError::VersionNotLocked
        );
        svc.lock(&id).await.unwrap();
        let payload = svc.resolve(&id, "runway", Modality::Image, None).await.unwrap();
        assert_eq!(payload.reference_images, vec!["face_a"]);
        assert_eq!(payload.lora_ref.as_deref(), Some("lora_y"));
    }

    #[tokio::test]
    async fn generation_provenance_requires_lock_and_is_recorded() {
        let svc = service();
        let created = svc.create(create_character()).await.unwrap();
        let id = created.view.passport.id;
        svc.lock(&id).await.unwrap();
        let rec = svc
            .record_generation(&id, 1, "heygen", Modality::Video, "r2://out/clip.mp4")
            .await
            .unwrap();
        assert_eq!(rec.passport_id, id);
        assert_eq!(rec.version, 1);
        let all = svc.generations(&id).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].output_ref, "r2://out/clip.mp4");
    }
}
