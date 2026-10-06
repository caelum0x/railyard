//! Live-Postgres test harness: a fresh database per test (created from
//! `RITHUB_TEST_DATABASE_URL`, migrated, dropped on [`Forge::finish`]), a temp
//! data root, and the real router. Without the variable each test returns
//! early and reports as passing, like the other live suites.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use rithub_core::PgStore;
use rithub_git::RepoHost;
use rithub_server::state::AppState;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;

pub struct Forge {
    pub app: Router,
    pub state: AppState,
    pub pool: PgPool,
    admin_url: String,
    db_name: String,
    pub data_root: std::path::PathBuf,
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }
}

fn swap_db(url: &str, db: &str) -> String {
    let (base, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let root = base.rsplit_once('/').map_or(base, |(r, _)| r);
    match query {
        Some(q) => format!("{root}/{db}?{q}"),
        None => format!("{root}/{db}"),
    }
}

/// A migrated, empty forge; `None` when no test database is configured.
pub async fn forge() -> Option<Forge> {
    let admin_url = std::env::var("RITHUB_TEST_DATABASE_URL").ok().filter(|v| !v.trim().is_empty())?;
    let db_name = format!("rithub_t_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new().max_connections(1).connect(&admin_url).await.expect("admin connection");
    sqlx::query(&format!("CREATE DATABASE {db_name}")).execute(&admin).await.expect("create test database");
    admin.close().await;
    let pool = PgPoolOptions::new().max_connections(5).connect(&swap_db(&admin_url, &db_name)).await.expect("test db connection");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("migrations");
    let data_root = std::env::temp_dir().join(&db_name);
    std::fs::create_dir_all(&data_root).expect("data root");
    let state = AppState::new(PgStore::new(pool.clone()), RepoHost::new(&data_root), None, data_root.clone());
    let app = rithub_server::build_router(state.clone());
    Some(Forge { app, state, pool, admin_url, db_name, data_root })
}

impl Forge {
    pub async fn send(&self, method: &str, uri: &str, auth: Option<&str>, body: Option<Vec<u8>>, content_type: &str) -> Resp {
        let mut b = Request::builder().method(method).uri(uri).header("host", "forge.test");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        let req = match body {
            Some(bytes) => b.header("content-type", content_type).body(Body::from(bytes)).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec();
        Resp { status, headers, bytes }
    }

    pub async fn api(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Resp {
        let auth = token.map(|t| format!("Bearer {t}"));
        self.send(method, uri, auth.as_deref(), body.map(|v| v.to_string().into_bytes()), "application/json").await
    }

    /// A user (returns its token).
    pub async fn user(&self, username: &str) -> String {
        let r = self.api("POST", "/api/users", None, Some(json!({ "username": username, "email": format!("{username}@x.test") }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.bytes));
        r.json()["token"].as_str().unwrap().to_string()
    }

    /// An org owned by `token`'s user with one repo.
    pub async fn org_and_repo(&self, token: &str, org: &str, repo: &str, private: bool) {
        let r = self.api("POST", "/api/orgs", Some(token), Some(json!({ "name": org }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.bytes));
        let visibility = if private { "private" } else { "public" };
        let r = self.api("POST", &format!("/api/orgs/{org}/repos"), Some(token), Some(json!({ "name": repo, "visibility": visibility }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.bytes));
    }

    /// Basic auth the way git sends it.
    pub fn basic(username: &str, token: &str) -> String {
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{token}")))
    }

    /// Drop the database and the data root.
    pub async fn finish(self) {
        self.pool.close().await;
        if let Ok(admin) = PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await {
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.db_name)).execute(&admin).await;
        }
        let _ = std::fs::remove_dir_all(&self.data_root);
    }
}
