//! One behaviour suite, run against MemStore always and against Postgres when
//! `SWITCHYARD_TEST_PG_ADMIN_URL` points at a server where a scratch database
//! can be created (e.g. `postgres://localhost/postgres`).

use chrono::{Duration, Utc};
use serde_json::json;
use switchyard_core::hash::verify_chain;
use switchyard_store::*;

fn new_call(project: &str, id: &str, cost: i64, key: &str, target: &str, side: &str) -> NewCall {
    NewCall {
        id: id.into(),
        project_id: project.into(),
        org_id: "org_1".into(),
        key_public_id: key.into(),
        target: target.into(),
        tool_id: Some(target.into()),
        provider_id: Some("p".into()),
        side_effect: side.into(),
        idempotency_key: None,
        request_hash: "h".into(),
        status: "ok".into(),
        http_status: 200,
        latency_ms: 12,
        cost_micros: cost,
        price_basis: "per_call".into(),
        mint_hold_id: None,
        mint_receipt: None,
        rago_event_id: None,
        cache: "miss".into(),
        fallback_chain: json!([]),
        created_at: Utc::now(),
    }
}

async fn exercise(s: &dyn Store) {
    let now = Utc::now();
    let org = s
        .create_org(Org { id: "org_1".into(), slug: "acme".into(), name: "Acme".into(), mint_account_id: Some("acct_1".into()), rago_subscription_id: None, created_at: now })
        .await
        .unwrap();
    assert!(s
        .create_org(Org { id: "org_2".into(), slug: "acme".into(), name: "Dup".into(), mint_account_id: None, rago_subscription_id: None, created_at: now })
        .await
        .is_err());
    let mut p = s
        .create_project(Project {
            id: "prj_1".into(),
            org_id: org.id.clone(),
            name: "default".into(),
            monthly_cap_micros: Some(20_000_000),
            daily_cap_micros: None,
            tool_daily_caps: [("rally.email.send".to_string(), 1_000)].into(),
            routing_policy: "free_first".into(),
            egress_extra_hosts: vec![],
            pinned_tools: vec!["weather.forecast".into()],
            daily_send_limit: Some(100),
            created_at: now,
        })
        .await
        .unwrap();
    p.daily_cap_micros = Some(2_000_000);
    s.update_project(p.clone()).await.unwrap();
    let got = s.get_project("prj_1").await.unwrap();
    assert_eq!(got.daily_cap_micros, Some(2_000_000));
    assert_eq!(got.tool_daily_caps.get("rally.email.send"), Some(&1_000));
    assert_eq!(s.list_projects("org_1").await.unwrap().len(), 1);

    s.upsert_member(Member { org_id: "org_1".into(), user_id: "u1".into(), role: "owner".into() }).await.unwrap();
    s.upsert_member(Member { org_id: "org_1".into(), user_id: "u1".into(), role: "admin".into() }).await.unwrap();
    assert_eq!(s.get_member("org_1", "u1").await.unwrap().unwrap().role, "admin");

    let key = ApiKey {
        public_id: "abcdefghijkl".into(),
        project_id: "prj_1".into(),
        description: "ci".into(),
        scopes: vec!["tools:read".into(), "tools:call".into()],
        tool_allow: Some(vec!["weather.forecast".into()]),
        per_key_daily_cap_micros: Some(5),
        extra_projects: vec![],
        revoked_at: None,
        created_at: now,
    };
    s.insert_api_key(key.clone()).await.unwrap();
    let mut k = s.get_api_key("abcdefghijkl").await.unwrap().unwrap();
    assert_eq!(k.tool_allow, key.tool_allow);
    k.revoked_at = Some(now);
    s.update_api_key(k).await.unwrap();
    assert!(s.get_api_key("abcdefghijkl").await.unwrap().unwrap().revoked_at.is_some());

    let ok1 = s.ensure_org_key(OrgKey { org_id: "org_1".into(), dek_wrapped: vec![1, 2, 3], kek_version: 1, created_at: now }).await.unwrap();
    let ok2 = s.ensure_org_key(OrgKey { org_id: "org_1".into(), dek_wrapped: vec![9], kek_version: 1, created_at: now }).await.unwrap();
    assert_eq!(ok1.dek_wrapped, ok2.dek_wrapped, "first DEK wins");

    for (i, id) in ["cred_a", "cred_b"].iter().enumerate() {
        s.insert_credential(CredentialRow {
            id: id.to_string(),
            org_id: "org_1".into(),
            provider_id: "brave".into(),
            name: format!("brave {i}"),
            kind: "api_key".into(),
            ciphertext: vec![i as u8],
            nonce: vec![0; 24],
            meta: json!({}),
            created_at: now + Duration::seconds(i as i64),
            last_ok_at: None,
            last_error: None,
        })
        .await
        .unwrap();
    }
    assert_eq!(s.find_credential("org_1", "brave").await.unwrap().unwrap().id, "cred_b", "newest wins");
    s.mark_credential("cred_b", false, Some("401".into())).await.unwrap();
    assert_eq!(s.get_credential("org_1", "cred_b").await.unwrap().unwrap().last_error.as_deref(), Some("401"));
    assert!(s.delete_credential("org_1", "cred_b").await.unwrap());
    assert!(!s.delete_credential("org_1", "cred_b").await.unwrap());
    assert_eq!(s.list_credentials("org_1").await.unwrap().len(), 1);

    let b = BundleRow {
        id: "lead-to-video".into(),
        org_id: "org_1".into(),
        name: "L2V".into(),
        description: "".into(),
        skill_md: "# x".into(),
        tool_ids: vec!["rally.email.verify".into()],
        credential_refs: vec![],
        version: 1,
        updated_at: now,
    };
    s.upsert_bundle(b.clone()).await.unwrap();
    assert_eq!(s.upsert_bundle(b).await.unwrap().version, 2);

    // Hash chain + atomic outbox + idempotency.
    let outbox = vec![OutboxItem::new("ob_1".into(), "rago_event", json!({"x": 1}), now)];
    let idem = IdempotencyRow { project_id: "prj_1".into(), key: "k1".into(), request_hash: "h".into(), http_status: 200, response: json!({"ok": true}), expires_at: now + Duration::hours(24) };
    let r1 = s.record_call(new_call("prj_1", "call_1", 100, "abcdefghijkl", "weather.forecast", "read"), outbox, Some(idem)).await.unwrap();
    let r2 = s.record_call(new_call("prj_1", "call_2", 250, "other", "rally.email.send", "send"), vec![], None).await.unwrap();
    let r3 = s.record_call(new_call("prj_1", "call_3", 0, "abcdefghijkl", "weather.forecast", "read"), vec![], None).await.unwrap();
    assert_eq!((r1.seq, r2.seq, r3.seq), (1, 2, 3));
    assert_eq!(r2.prev_hash, r1.row_hash);
    let chain = s.chain("prj_1", 3).await.unwrap();
    let payloads: Vec<_> = chain.iter().map(|r| r.call.hash_payload(r.seq)).collect();
    verify_chain(chain.iter().zip(payloads.iter()).map(|(r, p)| (r.prev_hash.as_str(), r.row_hash.as_str(), p))).unwrap();
    let recent = s.list_calls("prj_1", None, 2).await.unwrap();
    assert_eq!(recent.iter().map(|r| r.call.id.as_str()).collect::<Vec<_>>(), vec!["call_3", "call_2"]);
    let older = s.list_calls("prj_1", Some("call_2"), 10).await.unwrap();
    assert_eq!(older.iter().map(|r| r.call.id.as_str()).collect::<Vec<_>>(), vec!["call_1"]);
    assert_eq!(s.get_call("prj_1", "call_2").await.unwrap().unwrap().call.cost_micros, 250);

    let base = SpendFilter { project_id: "prj_1".into(), ..Default::default() };
    assert_eq!(s.spend_since(base.clone()).await.unwrap(), 350);
    assert_eq!(s.spend_since(SpendFilter { key_public_id: Some("abcdefghijkl".into()), ..base.clone() }).await.unwrap(), 100);
    assert_eq!(s.spend_since(SpendFilter { target: Some("rally.email.send".into()), ..base.clone() }).await.unwrap(), 250);
    assert_eq!(s.spend_since(SpendFilter { side_effect: Some("send".into()), count_calls: true, ..base.clone() }).await.unwrap(), 1);
    assert_eq!(s.spend_since(SpendFilter { since: Some(now + Duration::hours(1)), ..base }).await.unwrap(), 0);

    let usage = s.usage("prj_1", now - Duration::hours(1), now + Duration::hours(1), GroupBy::Tool).await.unwrap();
    let w = usage.iter().find(|u| u.group == "weather.forecast").unwrap();
    assert_eq!((w.calls, w.billed_calls, w.cost_micros), (2, 1, 100));

    assert!(s.get_idempotency("prj_1", "k1", now).await.unwrap().is_some());
    assert!(s.get_idempotency("prj_1", "k1", now + Duration::hours(25)).await.unwrap().is_none(), "24h window");

    let due = s.due_outbox(now + Duration::seconds(1), 10).await.unwrap();
    assert_eq!(due.len(), 1);
    s.mark_outbox("ob_1", false, Some("rago down".into()), now + Duration::minutes(1)).await.unwrap();
    assert!(s.due_outbox(now + Duration::seconds(1), 10).await.unwrap().is_empty());
    s.mark_outbox("ob_1", true, None, now).await.unwrap();
    assert!(s.due_outbox(now + Duration::hours(1), 10).await.unwrap().is_empty());

    s.record_stat("weather.forecast", true, 100, 0).await.unwrap();
    s.record_stat("weather.forecast", false, 300, 0).await.unwrap();
    let st = s.stats().await.unwrap();
    assert_eq!(st[0].calls, 2);
    assert!((st[0].success_rate() - 0.5).abs() < 1e-9);

    // OAuth
    s.insert_client(OAuthClient { client_id: "c1".into(), kind: "dcr".into(), metadata: json!({}), redirect_uris: vec!["http://127.0.0.1/cb".into()], created_at: now }).await.unwrap();
    assert!(s.get_client("c1").await.unwrap().is_some());
    let code = OAuthCode {
        code_hash: "ch".into(),
        client_id: "c1".into(),
        user_id: "u1".into(),
        org_id: "org_1".into(),
        project_id: "prj_1".into(),
        scopes: vec!["tools:read".into()],
        redirect_uri: "http://127.0.0.1/cb".into(),
        code_challenge: "x".into(),
        resource: "https://sy/mcp".into(),
        expires_at: now + Duration::minutes(5),
    };
    s.insert_code(code).await.unwrap();
    assert!(s.take_code("ch").await.unwrap().is_some());
    assert!(s.take_code("ch").await.unwrap().is_none(), "codes are single use");
    let g = OAuthGrant {
        id: "g1".into(),
        family_id: "fam1".into(),
        client_id: "c1".into(),
        user_id: "u1".into(),
        org_id: "org_1".into(),
        project_id: "prj_1".into(),
        scopes: vec![],
        refresh_hash: "rh1".into(),
        resource: "r".into(),
        expires_at: now + Duration::days(30),
        used_at: None,
        created_at: now,
    };
    s.insert_grant(g).await.unwrap();
    assert_eq!(s.find_grant_by_refresh("rh1").await.unwrap().unwrap().id, "g1");
    assert!(s.consume_grant("g1").await.unwrap());
    assert!(!s.consume_grant("g1").await.unwrap(), "second use is a replay");
    s.revoke_family("fam1").await.unwrap();
    assert!(s.family_revoked("fam1").await.unwrap());
    s.add_consent("fam1", "rally.email.send").await.unwrap();
    assert!(s.has_consent("fam1", "rally.email.send").await.unwrap());
    assert!(!s.has_consent("fam1", "menivor.video.render").await.unwrap());
    let mut d = DeviceCode {
        device_hash: "dh".into(),
        user_code: "ABCD-EFGH".into(),
        client_id: "sy-cli".into(),
        scopes: vec![],
        status: "pending".into(),
        user_id: None,
        org_id: None,
        project_id: None,
        expires_at: now + Duration::minutes(10),
        last_poll_at: None,
        interval_s: 5,
    };
    s.insert_device(d.clone()).await.unwrap();
    d.status = "approved".into();
    d.project_id = Some("prj_1".into());
    s.update_device(d).await.unwrap();
    assert_eq!(s.get_device_by_user_code("ABCD-EFGH").await.unwrap().unwrap().status, "approved");
    assert_eq!(s.get_device("dh").await.unwrap().unwrap().project_id.as_deref(), Some("prj_1"));

    s.upsert_byo_tool(ByoTool { id: "crm".into(), org_id: "org_1".into(), base_url: "https://crm.acme.io/api".into(), credential_id: None, header: "Authorization".into(), prefix: "Bearer ".into(), created_at: now }).await.unwrap();
    assert_eq!(s.list_byo_tools(Some("org_1")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn mem_store_conformance() {
    exercise(&MemStore::new()).await;
}

#[tokio::test]
async fn pg_store_conformance() {
    let Ok(admin) = std::env::var("SWITCHYARD_TEST_PG_ADMIN_URL") else {
        eprintln!("skipping: SWITCHYARD_TEST_PG_ADMIN_URL not set");
        return;
    };
    use sqlx::Connection;
    let db = format!("sy_test_{}", ulid::Ulid::new().to_string().to_lowercase());
    let mut conn = sqlx::PgConnection::connect(&admin).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {db}")).execute(&mut conn).await.unwrap();
    let url = match admin.rfind('/') {
        Some(i) => format!("{}/{db}", &admin[..i]),
        None => panic!("admin url needs a /database suffix"),
    };
    {
        let store = PgStore::connect(&url).await.unwrap();
        exercise(&store).await;
        // The audit log refuses edits.
        let err = sqlx::query("UPDATE calls SET cost_micros = 0").execute(store.pool()).await.unwrap_err();
        assert!(err.to_string().contains("append-only"), "{err}");
        store.pool().close().await;
    }
    sqlx::query(&format!("DROP DATABASE {db}")).execute(&mut conn).await.unwrap();
}
