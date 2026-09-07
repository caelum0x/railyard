//! Transport-codec + signing-intent interop tests (SPEC §3/§5): byte-for-byte
//! wire framing matching the agent `aitp-client`, and the two-encoding
//! separation between transport (declaration-order base64) and signing
//! (sorted-canonical blake3).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use mint_core::{op_hash, Micros};
use mint_protocol::codec::{decode_authorization, decode_challenge};
use mint_protocol::intent::{authorization_intent, compute_op_hash};
use mint_protocol::{encode_authorization, encode_challenge, Authorization, Challenge, Scheme};

fn sample_challenge() -> Challenge {
    Challenge {
        payee: "ait1qprovider".to_string(),
        amount_micros: Micros::new(4200),
        currency: "AIT".to_string(),
        resource: "POST /v1/chat/completions#llama-3.1-70b".to_string(),
        nonce: "9f2c".to_string(),
        expiry: 1_723_958_400_000,
        scheme: Scheme::Hold,
    }
}

fn sample_authorization() -> Authorization {
    Authorization {
        payer: "acct_201J0000000000000000000000".to_string(),
        payee: "ait1qprovider".to_string(),
        amount_micros: Micros::new(4200),
        nonce: "9f2c".to_string(),
        resource: "POST /v1/chat/completions#llama-3.1-70b".to_string(),
        expiry: 1_723_958_400_000,
        scheme: Scheme::Hold,
        idempotency_key: "req_1".to_string(),
        pubkey_id: "payer-key-1".to_string(),
        op_hash: "deadbeef".to_string(),
        sig: "c2ln".to_string(),
    }
}

#[test]
fn codec_roundtrip_matches_client_bytes() {
    // A Challenge encodes to base64url of compact, DECLARATION-ORDER JSON and
    // decodes back identically — the exact bytes the agent client emits.
    let challenge = sample_challenge();
    let header = encode_challenge(&challenge).unwrap();
    assert_eq!(decode_challenge(&header).unwrap(), challenge);

    // GOLDEN: the decoded JSON is declaration-order, compact, with the scheme
    // rename and a bare-int amount. This string is exactly what serializing the
    // client's `PaymentChallenge` (same fields/order) produces.
    let json = String::from_utf8(URL_SAFE_NO_PAD.decode(&header).unwrap()).unwrap();
    assert_eq!(
        json,
        r#"{"payee":"ait1qprovider","amount_micros":4200,"currency":"AIT","resource":"POST /v1/chat/completions#llama-3.1-70b","nonce":"9f2c","expiry":1723958400000,"scheme":"aitp-hold"}"#
    );

    // Same for the Authorization (round-trip + golden field order).
    let auth = sample_authorization();
    let ah = encode_authorization(&auth).unwrap();
    assert_eq!(decode_authorization(&ah).unwrap(), auth);
    let ajson = String::from_utf8(URL_SAFE_NO_PAD.decode(&ah).unwrap()).unwrap();
    assert_eq!(
        ajson,
        r#"{"payer":"acct_201J0000000000000000000000","payee":"ait1qprovider","amount_micros":4200,"nonce":"9f2c","resource":"POST /v1/chat/completions#llama-3.1-70b","expiry":1723958400000,"scheme":"aitp-hold","idempotency_key":"req_1","pubkey_id":"payer-key-1","op_hash":"deadbeef","sig":"c2ln"}"#
    );
}

#[test]
fn amount_micros_is_bare_i64() {
    let auth = sample_authorization();
    let json = serde_json::to_string(&auth).unwrap();
    assert!(json.contains(r#""amount_micros":4200"#));
    // Never a float.
    assert!(!json.contains("4200.0"));

    // Scheme renames and currency on the challenge.
    let cjson = serde_json::to_string(&sample_challenge()).unwrap();
    assert!(cjson.contains(r#""scheme":"aitp-hold""#));
    assert!(cjson.contains(r#""currency":"AIT""#));
    let tjson = serde_json::to_string(&Challenge {
        scheme: Scheme::Transfer,
        ..sample_challenge()
    })
    .unwrap();
    assert!(tjson.contains(r#""scheme":"aitp-transfer""#));
}

#[test]
fn op_hash_uses_sorted_canonical_not_transport() {
    let auth = sample_authorization();

    // The signing hash is over sorted-key canonical JSON of the pinned intent
    // subset (excludes op_hash/sig/pubkey_id), so it equals a hand-built sorted
    // hash of those fields...
    let expected = op_hash(&serde_json::json!({
        "payer": auth.payer,
        "payee": auth.payee,
        "amount_micros": auth.amount_micros.get(),
        "nonce": auth.nonce,
        "resource": auth.resource,
        "expiry": auth.expiry,
        "scheme": auth.scheme,
        "idempotency_key": auth.idempotency_key,
    }));
    assert_eq!(compute_op_hash(&auth), expected);

    // ...and it is NOT a hash over the transport (declaration-order, base64'd)
    // bytes — proving the two encodings are independent.
    let transport = URL_SAFE_NO_PAD
        .decode(encode_authorization(&auth).unwrap())
        .unwrap();
    let transport_hash = *blake3::hash(&transport).as_bytes();
    assert_ne!(compute_op_hash(&auth).as_bytes(), &transport_hash);
}

#[test]
fn intent_excludes_unsigned_fields() {
    // Mutating op_hash / sig / pubkey_id must NOT change the signed intent (they
    // are excluded), but mutating a signed field (amount) must.
    let base = sample_authorization();
    let mut same = base.clone();
    same.op_hash = "different".into();
    same.sig = "different".into();
    same.pubkey_id = "different".into();
    assert_eq!(authorization_intent(&base), authorization_intent(&same));

    let mut changed = base.clone();
    changed.amount_micros = Micros::new(1);
    assert_ne!(authorization_intent(&base), authorization_intent(&changed));
}
