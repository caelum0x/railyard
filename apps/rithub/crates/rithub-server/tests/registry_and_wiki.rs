//! Live forge suites: the package registry (generic, npm, OCI) and wikis.

mod common;

use axum::http::StatusCode;
use base64::Engine;
use common::{forge, Forge};
use serde_json::json;
use sha2::{Digest, Sha256, Sha512};

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn text(r: &common::Resp) -> String {
    String::from_utf8_lossy(&r.bytes).into_owned()
}

#[tokio::test]
async fn generic_files_publish_download_and_respect_permissions() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "tools", true).await;
    let auth = Forge::basic("ada", &ada);
    let url = "/api/packages/acme/tools/generic/cli/1.0.0/cli-macos.tar.gz";
    let up = f.send("PUT", url, Some(&auth), Some(b"binary".to_vec()), "application/octet-stream").await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", text(&up));
    assert_eq!(f.send("PUT", url, Some(&auth), Some(b"other".to_vec()), "").await.status, StatusCode::CONFLICT);
    let got = f.send("GET", url, Some(&format!("Bearer {ada}")), None, "").await;
    assert_eq!((got.status, got.bytes.clone()), (StatusCode::OK, b"binary".to_vec()));
    // Private: anonymous is asked to sign in; a stranger cannot read or write.
    let anon = f.send("GET", url, None, None, "").await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert!(anon.headers.get("www-authenticate").is_some());
    let eve = f.user("eve").await;
    assert_ne!(f.send("GET", url, Some(&Forge::basic("eve", &eve)), None, "").await.status, StatusCode::OK);
    assert_ne!(f.send("PUT", "/api/packages/acme/tools/generic/cli/2.0.0/x", Some(&Forge::basic("eve", &eve)), Some(b"x".to_vec()), "").await.status, StatusCode::CREATED);
    assert_eq!(f.send("PUT", "/api/packages/acme/tools/generic/../x/y", Some(&auth), Some(b"x".to_vec()), "").await.status.is_success(), false);
    let list = f.api("GET", "/api/packages/acme/tools", Some(&ada), None).await.json();
    assert_eq!(list["packages"][0], json!({ "kind": "generic", "name": "cli", "versions": ["1.0.0"] }));
    assert_eq!(f.send("DELETE", url, Some(&auth), None, "").await.status, StatusCode::NO_CONTENT);
    assert_eq!(f.send("GET", url, Some(&auth), None, "").await.status, StatusCode::NOT_FOUND);
    f.finish().await;
}

fn npm_doc(name: &str, version: &str, tarball: &[u8]) -> Vec<u8> {
    let file = format!("{}-{version}.tgz", name.rsplit('/').next().unwrap());
    json!({
        "name": name,
        "dist-tags": { "latest": version },
        "versions": { version: { "name": name, "version": version, "main": "index.js", "dependencies": {} } },
        "_attachments": { file: { "content_type": "application/octet-stream", "data": base64::engine::general_purpose::STANDARD.encode(tarball), "length": tarball.len() } },
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn npm_publish_install_and_immutability() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "ui", false).await;
    let bearer = format!("Bearer {ada}");
    let base = "/api/packages/acme/ui/npm";
    let tar1 = b"fake tarball 1".to_vec();
    let put = f.send("PUT", &format!("{base}/@acme%2fbutton"), Some(&bearer), Some(npm_doc("@acme/button", "1.0.0", &tar1)), "application/json").await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", text(&put));
    // The same version again is refused; a new version is added.
    assert_eq!(f.send("PUT", &format!("{base}/@acme%2fbutton"), Some(&bearer), Some(npm_doc("@acme/button", "1.0.0", b"x")), "application/json").await.status, StatusCode::CONFLICT);
    let tar2 = b"fake tarball 2".to_vec();
    assert_eq!(f.send("PUT", &format!("{base}/@acme%2fbutton"), Some(&bearer), Some(npm_doc("@acme/button", "1.1.0", &tar2)), "application/json").await.status, StatusCode::CREATED);

    // Public repo: anonymous install works.
    let doc = f.send("GET", &format!("{base}/@acme%2fbutton"), None, None, "").await.json();
    assert_eq!(doc["dist-tags"]["latest"], "1.1.0", "{doc}");
    let v1 = &doc["versions"]["1.0.0"];
    let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&tar1)));
    assert_eq!(v1["dist"]["integrity"], integrity);
    let tarball_url = v1["dist"]["tarball"].as_str().unwrap();
    assert!(tarball_url.ends_with("/api/packages/acme/ui/npm/@acme/button/-/button-1.0.0.tgz"), "{tarball_url}");
    let path = tarball_url.split("forge.test").nth(1).unwrap_or(tarball_url);
    let got = f.send("GET", path, None, None, "").await;
    assert_eq!((got.status, got.bytes.clone()), (StatusCode::OK, tar1));
    // Anonymous cannot publish; mismatched names are refused.
    assert_eq!(f.send("PUT", &format!("{base}/left-pad"), None, Some(npm_doc("left-pad", "1.0.0", b"x")), "application/json").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(f.send("PUT", &format!("{base}/left-pad"), Some(&bearer), Some(npm_doc("right-pad", "1.0.0", b"x")), "application/json").await.status, StatusCode::BAD_REQUEST);
    f.finish().await;
}

#[tokio::test]
async fn oci_push_and_pull_an_image() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "svc", true).await;
    let auth = Forge::basic("ada", &ada);
    assert_eq!(f.send("GET", "/v2/", None, None, "").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(f.send("GET", "/v2/", Some(&auth), None, "").await.status, StatusCode::OK);

    // Layer: chunked upload (POST, PATCH, PATCH, PUT ?digest).
    let layer = b"layer-bytes-part-1|part-2".to_vec();
    let start = f.send("POST", "/v2/acme/svc/web/blobs/uploads/", Some(&auth), None, "").await;
    assert_eq!(start.status, StatusCode::ACCEPTED, "{}", text(&start));
    let loc = start.headers.get("location").unwrap().to_str().unwrap().to_string();
    assert_eq!(f.send("PATCH", &loc, Some(&auth), Some(layer[..12].to_vec()), "application/octet-stream").await.status, StatusCode::ACCEPTED);
    assert_eq!(f.send("PATCH", &loc, Some(&auth), Some(layer[12..].to_vec()), "application/octet-stream").await.status, StatusCode::ACCEPTED);
    assert_eq!(f.send("PUT", &format!("{loc}?digest={}", sha(b"wrong")), Some(&auth), None, "").await.status, StatusCode::BAD_REQUEST);
    let fin = f.send("PUT", &format!("{loc}?digest={}", sha(&layer)), Some(&auth), None, "").await;
    assert_eq!(fin.status, StatusCode::CREATED, "{}", text(&fin));

    // Config: monolithic POST.
    let config = br#"{"architecture":"arm64","os":"linux"}"#.to_vec();
    let mono = f.send("POST", &format!("/v2/acme/svc/web/blobs/uploads/?digest={}", sha(&config)), Some(&auth), Some(config.clone()), "application/octet-stream").await;
    assert_eq!(mono.status, StatusCode::CREATED, "{}", text(&mono));

    // A manifest naming a blob the repo lacks is refused.
    let mt = "application/vnd.oci.image.manifest.v1+json";
    let bad = json!({ "schemaVersion": 2, "mediaType": mt, "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": sha(b"nope"), "size": 4 }, "layers": [] });
    assert_eq!(f.send("PUT", "/v2/acme/svc/web/manifests/v1", Some(&auth), Some(bad.to_string().into_bytes()), mt).await.status, StatusCode::BAD_REQUEST);
    let manifest = json!({ "schemaVersion": 2, "mediaType": mt,
        "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": sha(&config), "size": config.len() },
        "layers": [{ "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": sha(&layer), "size": layer.len() }] })
    .to_string()
    .into_bytes();
    let pushed = f.send("PUT", "/v2/acme/svc/web/manifests/v1", Some(&auth), Some(manifest.clone()), mt).await;
    assert_eq!(pushed.status, StatusCode::CREATED, "{}", text(&pushed));
    assert_eq!(pushed.headers.get("docker-content-digest").unwrap().to_str().unwrap(), sha(&manifest));

    // Pull by tag and digest; blobs; tags.
    let by_tag = f.send("GET", "/v2/acme/svc/web/manifests/v1", Some(&auth), None, "").await;
    assert_eq!((by_tag.status, by_tag.bytes.clone()), (StatusCode::OK, manifest.clone()));
    assert_eq!(by_tag.headers.get("content-type").unwrap(), mt);
    assert_eq!(f.send("GET", &format!("/v2/acme/svc/web/manifests/{}", sha(&manifest)), Some(&auth), None, "").await.bytes, manifest);
    assert_eq!(f.send("GET", &format!("/v2/acme/svc/web/blobs/{}", sha(&layer)), Some(&auth), None, "").await.bytes, layer);
    assert_eq!(f.send("GET", "/v2/acme/svc/web/tags/list", Some(&auth), None, "").await.json()["tags"], json!(["v1"]));
    // A stranger can neither pull nor push.
    let eve = f.user("eve").await;
    assert_ne!(f.send("GET", "/v2/acme/svc/web/manifests/v1", Some(&Forge::basic("eve", &eve)), None, "").await.status, StatusCode::OK);
    assert_ne!(f.send("POST", "/v2/acme/svc/web/blobs/uploads/", Some(&Forge::basic("eve", &eve)), None, "").await.status, StatusCode::ACCEPTED);
    // Delete by tag.
    assert_eq!(f.send("DELETE", "/v2/acme/svc/web/manifests/v1", Some(&auth), None, "").await.status, StatusCode::ACCEPTED);
    assert_eq!(f.send("GET", "/v2/acme/svc/web/manifests/v1", Some(&auth), None, "").await.status, StatusCode::NOT_FOUND);
    f.finish().await;
}

#[tokio::test]
async fn wiki_pages_have_history_locks_and_permissions() {
    let Some(f) = forge().await else { return };
    let ada = f.user("ada").await;
    f.org_and_repo(&ada, "acme", "docs", true).await;
    let w = "/api/orgs/acme/repos/docs/wiki";
    assert_eq!(f.api("GET", w, Some(&ada), None).await.json()["pages"], json!([]));
    let first = f.api("PUT", &format!("{w}/Getting%20Started"), Some(&ada), Some(json!({ "content": "# Hello\n\n<script>x</script>Install it." }))).await;
    assert_eq!(first.status, StatusCode::OK, "{}", text(&first));
    let rev1 = first.json()["revision"].as_str().unwrap().to_string();
    let page = f.api("GET", &format!("{w}/Getting%20Started"), Some(&ada), None).await.json();
    assert_eq!(page["title"], "Getting Started");
    assert!(page["html"].as_str().unwrap().contains("<h1>Hello</h1>"));
    assert!(!page["html"].as_str().unwrap().contains("<script>"), "raw HTML is dropped");
    // Edit based on rev1, then a stale edit is refused.
    let second = f.api("PUT", &format!("{w}/Getting%20Started"), Some(&ada), Some(json!({ "content": "# Hello\n\nv2", "revision": rev1, "message": "Second draft" }))).await;
    assert_eq!(second.status, StatusCode::OK);
    let stale = f.api("PUT", &format!("{w}/Getting%20Started"), Some(&ada), Some(json!({ "content": "lost", "revision": rev1 }))).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    f.api("PUT", &format!("{w}/FAQ"), Some(&ada), Some(json!({ "content": "Q?" }))).await;
    let hist = f.api("GET", &format!("{w}/Getting%20Started/history"), Some(&ada), None).await.json();
    let summaries: Vec<&str> = hist["history"].as_array().unwrap().iter().map(|h| h["summary"].as_str().unwrap()).collect();
    assert_eq!(summaries, vec!["Second draft", "Update Getting Started"], "FAQ's commit is not in this page's history");
    let old = f.api("GET", &format!("{w}/Getting%20Started?rev={rev1}"), Some(&ada), None).await.json();
    assert!(old["content"].as_str().unwrap().contains("Install it."));
    let names: Vec<String> = f.api("GET", w, Some(&ada), None).await.json()["pages"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, vec!["FAQ", "Getting-Started"]);
    // Permissions: a stranger sees nothing and cannot edit; bad names are refused.
    let eve = f.user("eve").await;
    assert_eq!(f.api("GET", w, Some(&eve), None).await.status, StatusCode::NOT_FOUND);
    assert_ne!(f.api("PUT", &format!("{w}/FAQ"), Some(&eve), Some(json!({ "content": "pwn" }))).await.status, StatusCode::OK);
    assert_eq!(f.api("PUT", &format!("{w}/..%2Fx"), Some(&ada), Some(json!({ "content": "x" }))).await.status, StatusCode::BAD_REQUEST);
    // The wiki is a git repository too (smart HTTP advertisement, same auth).
    let refs = f.send("GET", "/acme/docs.wiki.git/info/refs?service=git-upload-pack", Some(&Forge::basic("ada", &ada)), None, "").await;
    assert_eq!(refs.status, StatusCode::OK);
    assert!(text(&refs).contains("refs/heads/main"));
    assert_ne!(f.send("GET", "/acme/docs.wiki.git/info/refs?service=git-upload-pack", None, None, "").await.status, StatusCode::OK);
    // Delete.
    assert_eq!(f.api("DELETE", &format!("{w}/FAQ"), Some(&ada), None).await.status, StatusCode::OK);
    assert_eq!(f.api("GET", &format!("{w}/FAQ"), Some(&ada), None).await.status, StatusCode::NOT_FOUND);
    // A repository may not be named like a wiki.
    assert_eq!(f.api("POST", "/api/orgs/acme/repos", Some(&ada), Some(json!({ "name": "docs.wiki" }))).await.status, StatusCode::BAD_REQUEST);
    f.finish().await;
}
