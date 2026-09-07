//! A single in-memory store implementing both persistence ports.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;

use passport_core::{
    CreateOutcome, Passport, PassportError, PassportFilter, PassportStore, PassportVersion,
};
use passport_provenance::{GenerationRecord, ProvenanceLog};

#[derive(Default)]
struct State {
    passports: HashMap<String, Passport>,
    versions: HashMap<(String, i64), PassportVersion>,
    idempotency: HashMap<String, String>,
    generations: Vec<GenerationRecord>,
}

/// Lightweight, thread-safe in-memory store.
#[derive(Default)]
pub struct MemoryStore {
    state: RwLock<State>,
}

impl MemoryStore {
    fn read(&self) -> Result<std::sync::RwLockReadGuard<'_, State>, PassportError> {
        self.state
            .read()
            .map_err(|_| PassportError::Storage("memory store lock poisoned".into()))
    }

    fn write(&self) -> Result<std::sync::RwLockWriteGuard<'_, State>, PassportError> {
        self.state
            .write()
            .map_err(|_| PassportError::Storage("memory store lock poisoned".into()))
    }
}

#[async_trait]
impl PassportStore for MemoryStore {
    async fn create(
        &self,
        idempotency_key: &str,
        passport: &Passport,
        head: &PassportVersion,
    ) -> Result<CreateOutcome, PassportError> {
        let mut state = self.write()?;
        if let Some(existing_id) = state.idempotency.get(idempotency_key).cloned() {
            let passport = state
                .passports
                .get(&existing_id)
                .cloned()
                .ok_or_else(|| PassportError::Storage("idempotent passport missing".into()))?;
            let head = state
                .versions
                .get(&(existing_id, passport.version))
                .cloned()
                .ok_or_else(|| PassportError::Storage("idempotent head missing".into()))?;
            return Ok(CreateOutcome {
                passport,
                head,
                created: false,
            });
        }
        state
            .idempotency
            .insert(idempotency_key.to_string(), passport.id.clone());
        state
            .passports
            .insert(passport.id.clone(), passport.clone());
        state
            .versions
            .insert((passport.id.clone(), head.version), head.clone());
        Ok(CreateOutcome {
            passport: passport.clone(),
            head: head.clone(),
            created: true,
        })
    }

    async fn get_passport(&self, id: &str) -> Result<Option<Passport>, PassportError> {
        Ok(self.read()?.passports.get(id).cloned())
    }

    async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<PassportVersion>, PassportError> {
        Ok(self
            .read()?
            .versions
            .get(&(id.to_string(), version))
            .cloned())
    }

    async fn head_version(&self, id: &str) -> Result<Option<PassportVersion>, PassportError> {
        Ok(self
            .read()?
            .versions
            .values()
            .filter(|v| v.passport_id == id)
            .max_by_key(|v| v.version)
            .cloned())
    }

    async fn latest_locked_version(
        &self,
        id: &str,
    ) -> Result<Option<PassportVersion>, PassportError> {
        Ok(self
            .read()?
            .versions
            .values()
            .filter(|v| v.passport_id == id && v.is_locked())
            .max_by_key(|v| v.version)
            .cloned())
    }

    async fn upsert(
        &self,
        passport: &Passport,
        version: &PassportVersion,
    ) -> Result<(), PassportError> {
        let mut state = self.write()?;
        state
            .passports
            .insert(passport.id.clone(), passport.clone());
        state
            .versions
            .insert((passport.id.clone(), version.version), version.clone());
        Ok(())
    }

    async fn list_passports(
        &self,
        filter: &PassportFilter,
    ) -> Result<Vec<Passport>, PassportError> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1_000) as usize;
        let mut result: Vec<_> = self
            .read()?
            .passports
            .values()
            .filter(|p| {
                filter.owner.as_ref().is_none_or(|owner| &p.owner == owner)
                    && filter.kind.as_ref().is_none_or(|kind| &p.kind == kind)
            })
            .cloned()
            .collect();
        result.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        result.truncate(limit);
        Ok(result)
    }

    async fn ready(&self) -> Result<(), PassportError> {
        Ok(())
    }
}

#[async_trait]
impl ProvenanceLog for MemoryStore {
    async fn record(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        let mut state = self.write()?;
        if let Some(existing) = state
            .generations
            .iter()
            .find(|existing| existing.id == record.id)
        {
            return if existing == record {
                Ok(())
            } else {
                Err(PassportError::IdempotencyConflict)
            };
        }
        state.generations.push(record.clone());
        Ok(())
    }

    async fn list(&self, passport_id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        let mut out: Vec<_> = self
            .read()?
            .generations
            .iter()
            .filter(|r| r.passport_id == passport_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(out)
    }

    async fn get(&self, id: &str) -> Result<Option<GenerationRecord>, PassportError> {
        Ok(self
            .read()?
            .generations
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }
}
