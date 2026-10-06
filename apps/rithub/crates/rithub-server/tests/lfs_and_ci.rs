//! Live forge suites over the harness: Git LFS and the CI check-run flow.

mod common;

use axum::http::StatusCode;
use common::{forge, Forge};
use serde_json::json;
use sha2::{Digest, Sha256};

fn oid_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

const LFS: &str = "application/vnd.git-lfs+json";

#[tokio::test]
async fn lfs_upload_then_download_round_trips_with_auth() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "assets", true).await;
    let auth = Forge::basic("ada", &ada);
    let blob = b"a large binary asset".to_vec();
    let oid = oid_of(&blob);
    let batch = "/acme/assets.git/info/lfs/objects/batch";
    let body = |op: &str| json!({ "operation": op, "transfers": ["basic"], "objects": [{ "oid": oid, "size": blob.len() }] }).to_string().into_bytes();

    // Upload negotiation hands back an authenticated upload action.
    let up = f.send("POST", batch, Some(&auth), Some(body("upload")), LFS).await;
    assert_eq!(up.status, StatusCode::OK, "{}", String::from_utf8_lossy(&up.bytes));
    let action = up.json()["objects"][0]["actions"]["upload"].clone();
    let href = action["href"].as_str().unwrap();
    assert!(href.ends_with(&format!("/acme/assets.git/info/lfs/objects/{oid}")), "{href}");
    assert_eq!(action["header"]["Authorization"], auth);

    // A body that does not hash to the oid is refused; the right one is stored.
    let path = format!("/acme/assets.git/info/lfs/objects/{oid}");
    assert_eq!(f.send("PUT", &path, Some(&auth), Some(b"tampered".to_vec()), "application/octet-stream").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.send("PUT", &path, Some(&auth), Some(blob.clone()), "application/octet-stream").await.status, StatusCode::OK);

    // Uploading again is a no-op; download returns the bytes.
    let again = f.send("POST", batch, Some(&auth), Some(body("upload")), LFS).await;
    assert!(again.json()["objects"][0].get("actions").is_none());
    let down = f.send("POST", batch, Some(&auth), Some(body("download")), LFS).await;
    assert!(down.json()["objects"][0]["actions"]["download"]["href"].is_string());
    let got = f.send("GET", &path, Some(&auth), None, "").await;
    assert_eq!((got.status, got.bytes.clone()), (StatusCode::OK, blob.clone()));

    // The private repo's objects are invisible to anonymous and other users.
    assert_ne!(f.send("GET", &path, None, None, "").await.status, StatusCode::OK);
    let eve = f.user("eve").await;
    assert_ne!(f.send("GET", &path, Some(&Forge::basic("eve", &eve)), None, "").await.status, StatusCode::OK);
    assert_ne!(f.send("PUT", &path, Some(&Forge::basic("eve", &eve)), Some(blob.clone()), "").await.status, StatusCode::OK);
    // Unknown objects report a per-object 404; bad oids are rejected.
    let missing = json!({ "operation": "download", "objects": [{ "oid": "b".repeat(64), "size": 1 }, { "oid": "../x", "size": 1 }] });
    let m = f.send("POST", batch, Some(&auth), Some(missing.to_string().into_bytes()), LFS).await.json();
    assert_eq!(m["objects"][0]["error"]["code"], 404);
    assert_eq!(m["objects"][1]["error"]["code"], 422);
    f.finish().await;
}

#[tokio::test]
async fn a_runner_reports_check_runs_scoped_to_its_repo() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "app", false).await;
    f.org_and_repo(&ada, "beta", "other", false).await;
    let reg = f.api("POST", "/api/orgs/acme/repos/app/runners", Some(&ada), Some(json!({ "name": "linux-1" }))).await;
    assert_eq!(reg.status, StatusCode::OK, "{}", String::from_utf8_lossy(&reg.bytes));
    let runner = reg.json()["token"].as_str().unwrap().to_string();
    let sha = "a".repeat(40);
    let made = f.api("POST", "/api/orgs/acme/repos/app/check-runs", Some(&runner), Some(json!({ "name": "test", "head_sha": sha, "status": "in_progress" }))).await;
    assert_eq!(made.status, StatusCode::OK, "{}", String::from_utf8_lossy(&made.bytes));
    let id = made.json()["id"].as_str().unwrap().to_string();
    let done = f
        .api("PATCH", &format!("/api/orgs/acme/repos/app/check-runs/{id}"), Some(&runner), Some(json!({ "status": "completed", "conclusion": "success", "output_summary": "2 steps passed" })))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", String::from_utf8_lossy(&done.bytes));
    let list = f.api("GET", &format!("/api/orgs/acme/repos/app/commits/{sha}/check-runs"), Some(&ada), None).await.json();
    assert!(list.to_string().contains("success"), "{list}");
    // The token is useless on another repo.
    let other = f.api("POST", "/api/orgs/beta/repos/other/check-runs", Some(&runner), Some(json!({ "name": "x", "head_sha": sha }))).await;
    assert_eq!(other.status, StatusCode::FORBIDDEN);
    f.finish().await;
}

#[tokio::test]
async fn the_runner_executes_workflows_and_the_forge_shows_the_results() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "svc", false).await;
    let runner = f.api("POST", "/api/orgs/acme/repos/svc/runners", Some(&ada), Some(json!({ "name": "r1" }))).await.json()["token"]
        .as_str()
        .unwrap()
        .to_string();
    // Serve the forge on a real port for the runner's HTTP client.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = f.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let checkout = std::env::temp_dir().join(format!("rr-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(checkout.join(".rithub/workflows")).unwrap();
    std::fs::write(
        checkout.join(".rithub/workflows/ci.yml"),
        "name: CI\non: push\njobs:\n  unit:\n    steps:\n      - name: compile\n        run: echo built\n  lint:\n    steps:\n      - run: echo 'style error' && exit 1\n",
    )
    .unwrap();
    let sha = "c".repeat(40);
    let reporter = rithub_runner::HttpReporter::new(&format!("http://{addr}"), "acme", "svc", runner);
    let reports = rithub_runner::run_commit(&checkout, &sha, "push", &reporter).await.unwrap();
    assert_eq!(reports.len(), 2);

    let runs = f.api("GET", &format!("/api/orgs/acme/repos/svc/commits/{sha}/check-runs"), Some(&ada), None).await.json();
    let text = runs.to_string();
    assert!(text.contains("CI / unit") && text.contains("CI / lint"), "{runs}");
    assert!(text.contains("\"success\"") && text.contains("\"failure\""), "{runs}");
    assert!(text.contains("style error"), "the failing step's output is in the summary: {runs}");
    let _ = std::fs::remove_dir_all(&checkout);
    f.finish().await;
}
