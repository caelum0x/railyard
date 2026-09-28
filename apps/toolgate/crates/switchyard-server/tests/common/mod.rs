//! Test harness: the real router and pipeline over MemStore, in-memory Relay
//! keys, and the testkit's fake upstreams, Mint and Rago.

#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use chrono::Utc;
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use switchyard_auth::{KeyVerifier, MemRelayKeys, StaticIdentity, TokenSigner};
use switchyard_meter::OutboxWorker;
use switchyard_server::{router, AppState, Deps, Settings};
use switchyard_store::{ApiKey, MemStore, Member, Org, Project, Store};
use switchyard_testkit::{fixtures, FakeHttp, FakeMint, FakeRago};
use switchyard_vault::Vault;
use tower::ServiceExt;

pub const BASE: &str = "https://sy.test";
pub const ACCOUNT: &str = "acct_acme";
pub const REVENUE: &str = "acct_switchyard_revenue";

pub struct H {
    pub st: AppState,
    pub app: Router,
    pub http: Arc<FakeHttp>,
    pub mint: Arc<FakeMint>,
    pub rago: Arc<FakeRago>,
    pub store: Arc<dyn Store>,
    pub mem: Option<Arc<MemStore>>,
    pub keys: Arc<MemRelayKeys>,
    pub key: String,
    pub org: Org,
    pub project: Project,
}

pub fn project(org_id: &str) -> Project {
    Project {
        id: format!("prj_{}", ulid::Ulid::new().to_string().to_lowercase()),
        org_id: org_id.into(),
        name: "default".into(),
        monthly_cap_micros: None,
        daily_cap_micros: None,
        tool_daily_caps: Default::default(),
        routing_policy: "free_first".into(),
        egress_extra_hosts: vec![],
        pinned_tools: vec![],
        daily_send_limit: Some(100),
        created_at: Utc::now(),
    }
}

pub async fn harness() -> H {
    let mem = Arc::new(MemStore::new());
    harness_with(mem.clone(), Some(mem)).await
}

pub async fn harness_with(store: Arc<dyn Store>, mem: Option<Arc<MemStore>>) -> H {
    let mut settings = Settings::for_tests(BASE);
    settings.adapter_config = fixtures::test_config();
    settings.platform_credentials.insert("rally".into(), "platform-rally-token".into());
    settings.platform_credentials.insert("menivor".into(), "platform-menivor-token".into());
    let keys = Arc::new(MemRelayKeys::new());
    let http = FakeHttp::new();
    fixtures::install_all(&http);
    let mint = FakeMint::new();
    mint.fund(ACCOUNT, 10_000_000);
    let rago = FakeRago::new();
    let st = AppState::new(Deps {
        settings,
        catalog: fixtures::catalog(),
        store: store.clone(),
        keys: keys.clone(),
        identity: Arc::new(StaticIdentity::new(&[("sess-alice", "u_alice", "alice@acme.test"), ("sess-mallory", "u_mallory", "m@evil.test")])),
        vault: Vault::new(1, &Vault::generate_kek_b64()).unwrap(),
        http: http.clone(),
        mint: Some(mint.clone()),
        rago: Some(rago.clone()),
        signer: TokenSigner::new(None).unwrap(),
    })
    .unwrap();
    let suffix = ulid::Ulid::new().to_string().to_lowercase();
    let org = store
        .create_org(Org {
            id: format!("org_{suffix}"),
            slug: format!("acme-{}", &suffix[suffix.len() - 8..]),
            name: "Acme".into(),
            mint_account_id: Some(ACCOUNT.into()),
            rago_subscription_id: Some("sub_acme".into()),
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    let project = store.create_project(project(&org.id)).await.unwrap();
    store.upsert_member(Member { org_id: org.id.clone(), user_id: "u_alice".into(), role: "owner".into() }).await.unwrap();
    let key = mint_key(&keys, store.as_ref(), &org, &project.id, &["tools:read", "tools:call", "tools:write", "tools:send", "tools:spend", "admin"], None).await;
    let app = router(st.clone());
    H { st, app, http, mint, rago, store, mem, keys, key, org, project }
}

pub async fn mint_key(keys: &MemRelayKeys, store: &dyn Store, org: &Org, project_id: &str, scopes: &[&str], tool_allow: Option<Vec<String>>) -> String {
    let (raw, public_id) = keys.mint(&org.slug, project_id, "test").await.unwrap();
    store
        .insert_api_key(ApiKey {
            public_id,
            project_id: project_id.into(),
            description: "test".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            tool_allow,
            per_key_daily_cap_micros: None,
            extra_projects: vec![],
            revoked_at: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    raw.expose_secret().to_string()
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub text: String,
}

pub async fn send(app: &Router, method: &str, path: &str, token: Option<&str>, body: Option<Value>, extra: &[(&str, &str)]) -> Resp {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    raw(app, req).await
}

pub async fn raw(app: &Router, req: Request<Body>) -> Resp {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Resp { status, headers, body, text }
}

pub async fn call(h: &H, tool: &str, input: Value) -> Resp {
    send(&h.app, "POST", &format!("/v1/tools/{tool}/call"), Some(&h.key), Some(json!({ "input": input })), &[]).await
}

pub async fn drain(h: &H) -> switchyard_meter::outbox::DrainReport {
    OutboxWorker { store: h.store.clone(), rago: Some(h.rago.clone()), mint: Some(h.mint.clone()) }.drain_once().await
}
