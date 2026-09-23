//! Live-DB proof for the PRODUCTION Postgres store: the create/get round-trip,
//! the `INSERT … ON CONFLICT (idempotency_key) DO NOTHING` create-idempotency
//! path, concurrent create under the same key, and the provenance
//! (money/issuance) idempotency contract — all against REAL Postgres SQL.
//!
//! Gated on `PASSPORT_DATABASE_URL` (falling back to `DATABASE_URL`); when
//! neither is set the whole suite skips so DB-less CI stays green. Point it at a
//! throwaway Postgres, e.g.:
//!
//!   PASSPORT_DATABASE_URL=postgres://passport:passport@localhost:55441/passport \
//!     cargo test -p passport-store-postgres --test postgres_live -- --nocapture
//!
//! Every test scopes its rows behind a unique `owner`/`idempotency_key`, so the
//! suite is safe to run repeatedly against a shared database.

use chrono::{DateTime, Utc};

use passport_core::{
    new_passport_id, Passport, PassportFilter, PassportKind, PassportStore, PassportVersion,
    PassportAssets, Rights, VersionStatus,
};
use passport_provenance::{GenerationRecord, ProvenanceLog};
use passport_store_postgres::PostgresStore;

/// Connect + migrate, or `None` when no DB URL is configured (skip signal).
async fn connect() -> Option<PostgresStore> {
    let url = std::env::var("PASSPORT_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    let store = PostgresStore::connect(&url)
        .await
        .expect("connect to Postgres");
    store.migrate().await.expect("run migrations");
    Some(store)
}

/// Fetch a live store or bail out of the test as a skip.
macro_rules! store_or_skip {
    () => {
        match connect().await {
            Some(store) => store,
            None => {
                eprintln!(
                    "SKIP: set PASSPORT_DATABASE_URL (or DATABASE_URL) to run the live Postgres suite"
                );
                return;
            }
        }
    };
}

/// A per-invocation-unique owner so each test's rows are isolated and countable.
fn unique_owner() -> String {
    format!("owner-{}", new_passport_id())
}

/// A per-invocation-unique idempotency key.
fn unique_key() -> String {
    format!("idem-{}", new_passport_id())
}

fn new_passport(owner: &str, kind: PassportKind, now: DateTime<Utc>) -> Passport {
    Passport {
        id: new_passport_id(),
        kind,
        owner: owner.to_string(),
        status: VersionStatus::Draft,
        version: 1,
        created_at: now,
        updated_at: now,
        archived_at: None,
    }
}

fn draft_version(passport_id: &str, kind: PassportKind, now: DateTime<Utc>) -> PassportVersion {
    PassportVersion {
        passport_id: passport_id.to_string(),
        version: 1,
        status: VersionStatus::Draft,
        assets: PassportAssets::empty(kind),
        rights: Rights::default(),
        content_digest: None,
        created_at: now,
        updated_at: now,
        locked_at: None,
    }
}

async fn count_for_owner(store: &PostgresStore, owner: &str) -> usize {
    store
        .list_passports(&PassportFilter {
            owner: Some(owner.to_string()),
            kind: None,
            limit: Some(1_000),
            ..PassportFilter::default()
        })
        .await
        .expect("list_passports")
        .len()
}

/// create persists; get_passport / head_version / get_version read it back intact.
#[tokio::test]
async fn create_then_get_roundtrip_persists_fields() {
    let store = store_or_skip!();
    let owner = unique_owner();
    let key = unique_key();
    let now = Utc::now();

    let passport = new_passport(&owner, PassportKind::Character, now);
    let head = draft_version(&passport.id, PassportKind::Character, now);

    let outcome = store
        .create(&key, &passport, &head)
        .await
        .expect("create against real Postgres");
    assert!(outcome.created, "first create must report created = true");
    assert_eq!(outcome.passport, passport);
    assert_eq!(outcome.head, head);

    let fetched = store
        .get_passport(&passport.id)
        .await
        .expect("get_passport")
        .expect("passport row must exist after create");
    assert_eq!(fetched, passport, "persisted passport fields round-trip");

    let head_back = store
        .head_version(&passport.id)
        .await
        .expect("head_version")
        .expect("head version must exist");
    assert_eq!(head_back, head, "persisted head version round-trips");

    let v1 = store
        .get_version(&passport.id, 1)
        .await
        .expect("get_version")
        .expect("version 1 must exist");
    assert_eq!(v1, head);

    assert_eq!(count_for_owner(&store, &owner).await, 1);
}

/// Replaying the SAME idempotency_key returns the ORIGINAL passport with
/// created = false and inserts no duplicate row — the real ON CONFLICT contract.
#[tokio::test]
async fn idempotent_replay_returns_original_no_duplicate() {
    let store = store_or_skip!();
    let owner = unique_owner();
    let key = unique_key();
    let now = Utc::now();

    let first_passport = new_passport(&owner, PassportKind::Brand, now);
    let first_head = draft_version(&first_passport.id, PassportKind::Brand, now);
    let first = store
        .create(&key, &first_passport, &first_head)
        .await
        .expect("first create");
    assert!(first.created);

    // Same key, a DIFFERENT candidate passport id (as the server would generate).
    let replay_passport = new_passport(&owner, PassportKind::Brand, now);
    let replay_head = draft_version(&replay_passport.id, PassportKind::Brand, now);
    let replay = store
        .create(&key, &replay_passport, &replay_head)
        .await
        .expect("replay create");

    assert!(!replay.created, "replay must report created = false");
    assert_eq!(
        replay.passport, first_passport,
        "replay returns the ORIGINAL passport, not the new candidate"
    );
    assert_eq!(replay.head, first_head);
    assert_ne!(
        replay.passport.id, replay_passport.id,
        "the second candidate id must NOT have been persisted"
    );

    // Exactly one row for this owner: the candidate insert was discarded.
    assert_eq!(count_for_owner(&store, &owner).await, 1);
    assert!(
        store
            .get_passport(&replay_passport.id)
            .await
            .expect("get_passport")
            .is_none(),
        "the discarded candidate id must not exist"
    );
}

/// Two concurrent creates under the SAME idempotency_key: exactly one persists,
/// both callers observe a consistent winning passport, no duplicate, no error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_create_same_key_persists_exactly_one() {
    let store = store_or_skip!();
    let owner = unique_owner();
    let key = unique_key();
    let now = Utc::now();

    let s1 = store.clone();
    let s2 = store.clone();
    let (o1, k1) = (owner.clone(), key.clone());
    let (o2, k2) = (owner.clone(), key.clone());

    let t1 = tokio::spawn(async move {
        let p = new_passport(&o1, PassportKind::Persona, now);
        let v = draft_version(&p.id, PassportKind::Persona, now);
        s1.create(&k1, &p, &v).await
    });
    let t2 = tokio::spawn(async move {
        let p = new_passport(&o2, PassportKind::Persona, now);
        let v = draft_version(&p.id, PassportKind::Persona, now);
        s2.create(&k2, &p, &v).await
    });

    let r1 = t1.await.unwrap().expect("concurrent create 1");
    let r2 = t2.await.unwrap().expect("concurrent create 2");

    assert_ne!(
        r1.created, r2.created,
        "exactly one caller may report created = true"
    );
    assert_eq!(
        r1.passport.id, r2.passport.id,
        "both callers must converge on the same winning passport"
    );

    assert_eq!(
        count_for_owner(&store, &owner).await,
        1,
        "exactly one passport row may survive under the shared key"
    );
}

/// Provenance record() is idempotent on the generation id (money/issuance
/// semantics): same record replays cleanly to one row; a conflicting record
/// under the same id is rejected with IdempotencyConflict.
#[tokio::test]
async fn provenance_record_is_idempotent_and_detects_conflicts() {
    let store = store_or_skip!();
    let owner = unique_owner();
    let key = unique_key();
    let now = Utc::now();

    // A locked version is required to build a provenance record; persist it first
    // so the generation's FK to passports is satisfied.
    let passport = new_passport(&owner, PassportKind::Character, now);
    let draft = draft_version(&passport.id, PassportKind::Character, now);
    store
        .create(&key, &passport, &draft)
        .await
        .expect("create passport");
    let locked = draft.locked(now).expect("lock draft");
    store
        .upsert(&passport, &locked)
        .await
        .expect("upsert locked version");

    let record = GenerationRecord::new_with_id(
        format!("gen_ext_{}", new_passport_id()),
        &locked,
        "runway",
        "image",
        "r2://asset/hero.png",
        Some("sha256:asset".into()),
        now,
    );

    store.record(&record).await.expect("first record");
    // Exact replay: ON CONFLICT (id) DO NOTHING + equality check => Ok, one row.
    store.record(&record).await.expect("idempotent replay");

    let listed = store.list(&passport.id).await.expect("list generations");
    assert_eq!(listed.len(), 1, "idempotent replay must not duplicate");
    assert_eq!(listed[0], record);

    // Same id, different payload => real conflict.
    let mut conflicting = record.clone();
    conflicting.output_ref = "r2://asset/other.png".into();
    let err = store
        .record(&conflicting)
        .await
        .expect_err("conflicting record under same id must error");
    assert_eq!(err, passport_core::PassportError::IdempotencyConflict);

    // Still exactly one row after the rejected conflict.
    assert_eq!(store.list(&passport.id).await.unwrap().len(), 1);
}

/// upsert of a locked head is readable via latest_locked_version (issuance path).
#[tokio::test]
async fn upsert_exposes_latest_locked_version() {
    let store = store_or_skip!();
    let owner = unique_owner();
    let key = unique_key();
    let now = Utc::now();

    let passport = new_passport(&owner, PassportKind::Character, now);
    let draft = draft_version(&passport.id, PassportKind::Character, now);
    store
        .create(&key, &passport, &draft)
        .await
        .expect("create");

    assert!(
        store
            .latest_locked_version(&passport.id)
            .await
            .expect("latest_locked_version")
            .is_none(),
        "a fresh draft has no locked version yet"
    );

    let locked = draft.locked(now).expect("lock");
    let locked_passport = Passport {
        status: VersionStatus::Locked,
        updated_at: now,
        ..passport.clone()
    };
    store
        .upsert(&locked_passport, &locked)
        .await
        .expect("upsert locked");

    let got = store
        .latest_locked_version(&passport.id)
        .await
        .expect("latest_locked_version")
        .expect("locked version must be visible");
    assert_eq!(got, locked);
    assert!(got.content_digest.is_some(), "lock stamps a content digest");
}
