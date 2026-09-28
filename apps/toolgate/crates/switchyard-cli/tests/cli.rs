//! The CLI against a live local Switchyard (real router over fakes).

use std::io::Write;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use clap::Parser;
use secrecy::ExposeSecret;
use serde_json::json;
use switchyard_auth::{KeyVerifier, MemRelayKeys, StaticIdentity, TokenSigner};
use switchyard_cli::{run, Cli, Io};
use switchyard_server::{router, AppState, Deps, Settings};
use switchyard_store::{ApiKey, MemStore, Member, Org, Project, Store};
use switchyard_testkit::{fixtures, FakeHttp, FakeMint, FakeRago};
use switchyard_vault::Vault;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);
impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Buf {
    fn take(&self) -> String {
        String::from_utf8(std::mem::take(&mut *self.0.lock().unwrap())).unwrap()
    }
}

struct Env {
    url: String,
    key: String,
    dir: std::path::PathBuf,
    org: String,
    project: String,
}

async fn start() -> Env {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut settings = Settings::for_tests(&url);
    settings.adapter_config = fixtures::test_config();
    settings.platform_credentials.insert("rally".into(), "platform".into());
    let store = Arc::new(MemStore::new());
    let keys = Arc::new(MemRelayKeys::new());
    let http = FakeHttp::new();
    fixtures::install_all(&http);
    let mint = FakeMint::new();
    mint.fund("acct_cli", 5_000_000);
    let st = AppState::new(Deps {
        settings,
        catalog: fixtures::catalog(),
        store: store.clone(),
        keys: keys.clone(),
        identity: Arc::new(StaticIdentity::new(&[("sess-cli", "u_cli", "cli@acme.test")])),
        vault: Vault::new(1, &Vault::generate_kek_b64()).unwrap(),
        http,
        mint: Some(mint),
        rago: Some(FakeRago::new()),
        signer: TokenSigner::new(None).unwrap(),
    })
    .unwrap();
    let org = store
        .create_org(Org { id: "org_cli".into(), slug: "cli".into(), name: "CLI".into(), mint_account_id: Some("acct_cli".into()), rago_subscription_id: None, created_at: Utc::now() })
        .await
        .unwrap();
    let p = store
        .create_project(Project {
            id: "prj_cli".into(),
            org_id: org.id.clone(),
            name: "default".into(),
            monthly_cap_micros: None,
            daily_cap_micros: None,
            tool_daily_caps: Default::default(),
            routing_policy: "free_first".into(),
            egress_extra_hosts: vec![],
            pinned_tools: vec![],
            daily_send_limit: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    store.upsert_member(Member { org_id: org.id.clone(), user_id: "u_cli".into(), role: "owner".into() }).await.unwrap();
    let (raw, public_id) = keys.mint("cli", &p.id, "t").await.unwrap();
    store
        .insert_api_key(ApiKey {
            public_id,
            project_id: p.id.clone(),
            description: "".into(),
            scopes: vec!["admin".into()],
            tool_allow: None,
            per_key_daily_cap_micros: None,
            extra_projects: vec![],
            revoked_at: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    tokio::spawn(async move { axum::serve(listener, router(st)).await.unwrap() });
    let dir = std::env::temp_dir().join(format!("sy-cli-{}", std::process::id() as u64 * 1000 + rand_suffix()));
    Env { url, key: raw.expose_secret().to_string(), dir, org: org.id, project: p.id }
}

fn rand_suffix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as u64 % 1000
}

async fn sy(env: &Env, token: bool, args: &[&str], stdin: &str) -> (anyhow::Result<i32>, String) {
    let buf = Buf::default();
    let mut io = Io {
        stdin: Box::new(std::io::Cursor::new(stdin.as_bytes().to_vec())),
        out: Box::new(buf.clone()),
        config_dir: env.dir.clone(),
        env_token: token.then(|| env.key.clone()),
        env_url: token.then(|| env.url.clone()),
        open_browser: false,
    };
    let mut argv = vec!["sy"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(argv).expect("args parse");
    let r = run(cli, &mut io).await;
    (r, buf.take())
}

#[tokio::test]
async fn commands_against_a_live_server() {
    let env = start().await;
    let (r, out) = sy(&env, true, &["search", "weather", "forecast"], "").await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("weather.forecast") && out.contains("[free]"), "{out}");

    let (r, out) = sy(&env, true, &["call", "weather.forecast", "--input", r#"{"lat":52.5,"lon":13.4}"#], "").await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("via open-meteo") && out.contains("$0.00"), "{out}");

    let (r, out) = sy(&env, true, &["call", "rally.company.enrich", "--input", "-"], r#"{"domain":"stripe.com"}"#).await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("$0.005000"), "{out}");

    let (r, _) = sy(&env, true, &["call", "weather.forecast", "--input", r#"{"lat":500}"#], "").await;
    let err = r.unwrap_err().to_string();
    assert!(err.contains("invalid_input") && err.contains("/lat"), "{err}");

    let (r, out) = sy(&env, true, &["call", "rally.email.send", "--dry-run", "--input", r#"{"from":"a@acme.com","to":["x@y.io","z@y.io"]}"#], "").await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("would hold $0.000800"), "{out}");

    let (_, out) = sy(&env, true, &["balance"], "").await;
    assert!(out.contains("Balance $4.995000"), "{out}");

    let (r, out) = sy(&env, true, &["cred", "add", "brave", "--value-stdin"], "brave-secret\n").await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("write-only") && !out.contains("brave-secret"), "{out}");
    let (_, out) = sy(&env, true, &["cred", "ls"], "").await;
    assert!(out.contains("brave") && !out.contains("brave-secret"));

    let (_, out) = sy(&env, true, &["keys", "create", "--scopes", "tools:read,tools:call", "--tools", "weather.forecast", "--daily-cap", "1"], "").await;
    assert!(out.starts_with("sk-t0-"), "{out}");

    let (_, out) = sy(&env, true, &["project", "cap", "--daily", "2", "--monthly", "20"], "").await;
    assert!(out.contains("monthly $20.00") && out.contains("daily $2.00"), "{out}");

    let bdir = env.dir.join("skills/my-flow");
    std::fs::create_dir_all(&bdir).unwrap();
    std::fs::write(bdir.join("SKILL.md"), "---\nname: my-flow\ndescription: Enrich then verify\n---\n# My flow\nUse rally.company.enrich.\n").unwrap();
    std::fs::write(bdir.join("bundle.yaml"), "tool_ids: [rally.company.enrich, rally.email.verify]\ncredential_refs: [rally]\n").unwrap();
    let (r, out) = sy(&env, true, &["bundle", "push", bdir.to_str().unwrap()], "").await;
    assert_eq!(r.unwrap(), 0, "{out}");
    assert!(out.contains("Pushed my-flow v1"), "{out}");
    let install = env.dir.join("installed");
    let (_, out) = sy(&env, true, &["bundle", "install", "lead-to-video", "--dir", install.to_str().unwrap()], "").await;
    assert!(out.contains("Installed"));
    assert!(std::fs::read_to_string(install.join("lead-to-video/SKILL.md")).unwrap().contains("Lead to video"));

    let (_, out) = sy(&env, true, &["usage", "--by", "provider", "--since", "1d"], "").await;
    assert!(out.contains("rally") && out.contains("Total $0.005000"), "{out}");

    let lines = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
    ]
    .join("\n");
    let (r, out) = sy(&env, true, &["mcp"], &lines).await;
    assert_eq!(r.unwrap(), 0);
    let replies: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 2, "the notification gets no reply");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "search_tools");
}

#[tokio::test]
async fn device_login_stores_a_private_refreshable_token() {
    let env = start().await;
    let buf = Buf::default();
    let (url, dir) = (env.url.clone(), env.dir.clone());
    let out_buf = buf.clone();
    let login = tokio::spawn(async move {
        let mut io = Io { stdin: Box::new(std::io::Cursor::new(Vec::new())), out: Box::new(out_buf), config_dir: dir, env_token: None, env_url: None, open_browser: false };
        run(Cli::try_parse_from(["sy", "login", "--url", &url]).unwrap(), &mut io).await
    });
    // The human reads the code from the terminal and approves it in the browser.
    let mut code = None;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        if let Some(i) = text.find("confirm the code ") {
            code = Some(text[i + 17..i + 26].to_string());
            break;
        }
    }
    let code = code.expect("CLI printed a user code");
    let http = reqwest::Client::new();
    let page = http.get(format!("{}/device?user_code={code}", env.url)).header("cookie", "scribase_session=sess-cli").send().await.unwrap().text().await.unwrap();
    let csrf = hidden(&page, "csrf");
    let project = format!("{}|{}", env.org, env.project);
    let form = [("user_code", code.as_str()), ("csrf", csrf.as_str()), ("project", project.as_str()), ("scope_tools:read", "1"), ("scope_tools:call", "1"), ("decision", "approve")];
    let r = http.post(format!("{}/device", env.url)).header("cookie", "scribase_session=sess-cli").form(&form).send().await.unwrap().text().await.unwrap();
    assert!(r.contains("Connected"), "{r}");
    assert_eq!(login.await.unwrap().unwrap(), 0);
    assert!(buf.take().contains("Signed in"));

    let path = env.dir.join("credentials.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(stored["refresh_token"].as_str().unwrap().len() > 20);
    assert!(!stored.to_string().contains("sk-t0-"), "no raw key is stored");
    // Commands now work without SWITCHYARD_TOKEN.
    let (r, out) = sy(&env, false, &["search", "dns", "records"], "").await;
    assert_eq!(r.unwrap(), 0);
    assert!(out.contains("domain.dns"), "{out}");
    // An expired access token is refreshed (and rotated) transparently.
    let mut s: switchyard_cli::Stored = serde_json::from_value(stored).unwrap();
    let old_refresh = s.refresh_token.clone();
    s.expires_at = 0;
    s.save(&env.dir).unwrap();
    let (r, _) = sy(&env, false, &["balance"], "").await;
    assert_eq!(r.unwrap(), 0);
    let refreshed: switchyard_cli::Stored = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(refreshed.expires_at > Utc::now().timestamp());
    assert_ne!(refreshed.refresh_token, old_refresh);
    let (r, _) = sy(&env, false, &["logout"], "").await;
    assert_eq!(r.unwrap(), 0);
    assert!(!path.exists());
}

fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let i = html.find(&marker).unwrap() + marker.len();
    html[i..i + html[i..].find('"').unwrap()].to_string()
}
