//! End-to-end library flow: build a signed manifest, round-trip it through the
//! text format, verify its signature, then prove the scanner catches tampering
//! (modified + added + removed) and that editing a recorded hash breaks the MAC.

use ranguard::manifest::Manifest;
use ranguard::report::ChangeKind;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("ranguard_it_{tag}_{nanos}"));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_file(dir: &Path, name: &str, contents: &[u8]) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut f = fs::File::create(path).unwrap();
    f.write_all(contents).unwrap();
}

#[test]
fn snapshot_roundtrip_verify_then_detect_tampering() {
    let key = b"integration-signing-key";
    let dir = temp_dir("flow");
    write_file(&dir, "config.cfg", b"fov=90\n");
    write_file(&dir, "assets/textures.pak", &[0xAB; 2048]);
    write_file(&dir, "scripts/init.lua", b"print('ok')\n");

    // Build -> serialize -> deserialize preserves the manifest exactly.
    let manifest = Manifest::build(&dir, key).unwrap();
    let text = manifest.serialize();
    let parsed = Manifest::deserialize(&text).unwrap();
    assert_eq!(parsed.entries.len(), 3);
    assert_eq!(parsed.entries, manifest.entries);
    assert_eq!(parsed.mac_hex, manifest.mac_hex);

    // Signature verifies under the right key and fails under the wrong one.
    assert!(parsed.verify_signature(key));
    assert!(!parsed.verify_signature(b"wrong-key"));

    // A clean re-scan reports no changes.
    let clean = parsed.scan(&dir).unwrap();
    assert!(clean.is_clean());
    assert_eq!(clean.files_checked, 3);

    // Tamper: modify one, add one, remove one -> exactly three findings.
    write_file(&dir, "config.cfg", b"fov=90\nwallhack=1\n");
    write_file(&dir, "cheat.dll", b"payload");
    fs::remove_file(dir.join("scripts/init.lua")).unwrap();

    let dirty = parsed.scan(&dir).unwrap();
    assert_eq!(dirty.changes.len(), 3);
    let kinds: Vec<_> = dirty.changes.iter().map(|c| c.kind.clone()).collect();
    assert!(kinds.contains(&ChangeKind::Modified));
    assert!(kinds.contains(&ChangeKind::Added));
    assert!(kinds.contains(&ChangeKind::Removed));

    // Silently editing a recorded hash invalidates the signature.
    let mut forged = parsed.clone();
    forged.entries[0].sha256_hex = "0".repeat(64);
    assert!(!forged.verify_signature(key));

    fs::remove_dir_all(&dir).ok();
}
