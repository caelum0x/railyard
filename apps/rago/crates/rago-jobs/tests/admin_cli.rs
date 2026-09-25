// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! `rago-worker create-org` / `create-api-key` against an isolated migrated
//! Postgres: the org and key rows land, the printed key authenticates to its
//! org, and bad input fails without writing anything.

use rago_db::Db;
use rago_jobs::admin_cli::{self, AdminCommand, AdminError};
use sqlx::PgPool;

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

async fn run(db: &Db, v: &[&str]) -> Result<admin_cli::CreatedKey, AdminError> {
    let cmd: AdminCommand = admin_cli::parse_args(&argv(v)).expect("parse").expect("a subcommand");
    admin_cli::run(db, &cmd).await
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_org_mints_an_org_and_a_working_key(pool: PgPool) {
    let db = Db::new(pool);
    let created = run(&db, &["create-org", "--name", "Rally AutoGTM", "--currency", "eur", "--email", "ops@rally.dev"])
        .await
        .unwrap();
    assert_eq!(created.organization_slug, "rally-autogtm");
    assert_eq!(created.api_key_name.as_deref(), Some("default"));

    let (key, org) = db.authenticate(&created.api_key_value).await.unwrap().expect("key authenticates");
    assert_eq!(org.id, created.organization_id);
    assert_eq!(key.id, created.api_key_id);
    assert_eq!(org.name, "Rally AutoGTM");
    assert_eq!(org.default_currency, "EUR");
    assert_eq!(org.email.as_deref(), Some("ops@rally.dev"));
    assert!(org.hmac_key.is_some(), "webhook signing key is set");
    assert!(!key.is_expired(chrono::Utc::now()));

    let out = admin_cli::render(&created);
    assert_eq!(out.matches(&created.api_key_value).count(), 1, "key printed exactly once");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_org_refuses_a_taken_slug(pool: PgPool) {
    let db = Db::new(pool);
    run(&db, &["create-org", "--name", "OpenJev"]).await.unwrap();
    let err = run(&db, &["create-org", "--name", "Open Jev", "--slug", "openjev"]).await.unwrap_err();
    assert!(matches!(err, AdminError::SlugTaken(ref s) if s == "openjev"), "{err}");
    let orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM organizations").fetch_one(&db.pool).await.unwrap();
    let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys").fetch_one(&db.pool).await.unwrap();
    assert_eq!((orgs, keys), (1, 1), "nothing written on conflict");
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_api_key_by_slug_and_by_id(pool: PgPool) {
    let db = Db::new(pool);
    let org = run(&db, &["create-org", "--name", "Menivor"]).await.unwrap();

    let by_slug = run(&db, &["create-api-key", "--org", "menivor", "--name", "relay"]).await.unwrap();
    let id = org.organization_id.to_string();
    let by_id = run(&db, &["create-api-key", "--org", &id]).await.unwrap();

    for k in [&by_slug, &by_id] {
        assert_eq!(k.organization_id, org.organization_id);
        assert_eq!(k.organization_slug, "menivor");
        let (_, authed_org) = db.authenticate(&k.api_key_value).await.unwrap().expect("key authenticates");
        assert_eq!(authed_org.id, org.organization_id);
    }
    assert_eq!(by_slug.api_key_name.as_deref(), Some("relay"));
    assert_eq!(by_id.api_key_name, None);
    assert_ne!(by_slug.api_key_value, by_id.api_key_value);
    assert_ne!(by_slug.api_key_value, org.api_key_value);

    let keys = db.list_api_keys(org.organization_id).await.unwrap();
    assert_eq!(keys.len(), 3);
}

#[sqlx::test(migrator = "rago_db::MIGRATOR")]
async fn create_api_key_for_unknown_org_fails(pool: PgPool) {
    let db = Db::new(pool);
    let err = run(&db, &["create-api-key", "--org", "nobody"]).await.unwrap_err();
    assert!(matches!(err, AdminError::OrgNotFound(_)), "{err}");
    let err = run(&db, &["create-api-key", "--org", "00000000-0000-0000-0000-000000000001"]).await.unwrap_err();
    assert!(matches!(err, AdminError::OrgNotFound(_)), "{err}");
    let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys").fetch_one(&db.pool).await.unwrap();
    assert_eq!(keys, 0);
}
