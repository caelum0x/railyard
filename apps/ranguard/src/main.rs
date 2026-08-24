//! Demo driver for the Vanguard-style Integrity Monitor.
//!
//! DEFENSIVE / USERSPACE / EDUCATIONAL. Everything here operates on a sandbox
//! this tool creates and owns. It does NOT install a kernel driver, does NOT
//! read other processes' memory, and does NOT do anti-debugging. See README.
//!
//! Usage:
//!   ranguard                 Run the full end-to-end demo.
//!   ranguard demo            Same as above.
//!   ranguard game-loop [ms]  Internal: the tool's own "game" child process.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ranguard::heartbeat::{Client, Outcome, Server};
use ranguard::manifest::Manifest;
use ranguard::process::{AttestOutcome, GameProcess};

// Clearly-labeled DEMO key. In a real tool this would come from a secret
// manager / provisioning step, never be hardcoded (see README SAFETY section).
const DEMO_SIGNING_KEY: &[u8] = b"ranguard-demo-signing-key-not-a-real-secret";
const DEMO_HEARTBEAT_KEY: &[u8] = b"ranguard-demo-heartbeat-key-not-a-real-secret";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("demo");

    match cmd {
        "game-loop" => run_game_loop(&args),
        "demo" => {
            if let Err(e) = run_demo() {
                eprintln!("demo failed: {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: ranguard [demo | game-loop <ms>]");
            std::process::exit(2);
        }
    }
}

/// The tool's OWN "game" child process. It just idles so the attestation demo
/// has a live, tool-owned process to verify. It touches nothing else.
fn run_game_loop(args: &[String]) {
    let ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1500);
    let deadline = Duration::from_millis(ms);
    let start = SystemTime::now();
    while SystemTime::now().duration_since(start).unwrap_or_default() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn section(title: &str) {
    println!("\n=== {title} ===");
}

fn run_demo() -> std::io::Result<()> {
    println!("Vanguard-style Integrity Monitor — DEFENSIVE / USERSPACE / EDUCATIONAL");
    println!("This tool only inspects a sandbox it creates and a child it spawns.");
    println!("No kernel driver. No foreign-process memory scanning. No anti-debugging.");

    // ------------------------------------------------------------------
    // 0. Build a fresh, isolated sandbox seeded from the project's assets.
    //    We copy rather than mutate the repo so runs are repeatable and the
    //    checked-in assets stay pristine.
    // ------------------------------------------------------------------
    let seed_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let sandbox = make_sandbox(&seed_dir)?;
    println!("\nSandbox (tool-owned): {}", sandbox.display());

    // ------------------------------------------------------------------
    // 1. Integrity manifest at "trusted" time.
    // ------------------------------------------------------------------
    section("1. Build signed integrity manifest (trusted snapshot)");
    let manifest = Manifest::build(&sandbox, DEMO_SIGNING_KEY)?;
    let manifest_path = sandbox
        .parent()
        .unwrap_or(&sandbox)
        .join("ranguard.manifest");
    fs::write(&manifest_path, manifest.serialize())?;
    println!("Hashed {} asset file(s) with SHA-256:", manifest.entries.len());
    for e in &manifest.entries {
        println!("  {:<20} {} bytes  {}…", e.rel_path, e.size, &e.sha256_hex[..16]);
    }
    println!("Manifest signed (HMAC-SHA256): {}…", &manifest.mac_hex[..16]);
    println!("Manifest written to: {}", manifest_path.display());

    // ------------------------------------------------------------------
    // 2. Clean re-scan (should be OK).
    // ------------------------------------------------------------------
    section("2. Re-scan an untampered sandbox");
    let clean = manifest.scan(&sandbox)?;
    print!("{}", clean.render());

    // ------------------------------------------------------------------
    // 3. Tamper: modify one file, add one, remove one.
    // ------------------------------------------------------------------
    section("3. Tamper with the sandbox, then re-scan");
    tamper(&sandbox)?;
    println!("Applied tampering: modified config.cfg, added cheat.dll, removed textures.pak");
    let dirty = manifest.scan(&sandbox)?;
    print!("{}", dirty.render());

    // ------------------------------------------------------------------
    // 4. Verify the manifest itself is protected by its signature.
    // ------------------------------------------------------------------
    section("4. Verify the manifest's own signature (defends the manifest)");
    let loaded = Manifest::deserialize(&fs::read_to_string(&manifest_path)?)?;
    println!(
        "Loaded manifest signature valid with correct key : {}",
        loaded.verify_signature(DEMO_SIGNING_KEY)
    );
    println!(
        "Loaded manifest signature valid with wrong key   : {}",
        loaded.verify_signature(b"attacker-guessed-key")
    );
    // Simulate an attacker silently editing a recorded hash in the manifest.
    let mut forged = loaded.clone();
    if let Some(first) = forged.entries.first_mut() {
        first.sha256_hex = "0".repeat(64);
    }
    println!(
        "Tampered manifest (edited hash) signature valid  : {}  <- rejected",
        forged.verify_signature(DEMO_SIGNING_KEY)
    );

    // ------------------------------------------------------------------
    // 5. Process attestation over our OWN spawned child.
    // ------------------------------------------------------------------
    section("5. Process attestation (own child only, never arbitrary PIDs)");
    let self_exe = std::env::current_exe()?;
    let mut game = GameProcess::launch(&self_exe, &["game-loop", "1200"])?;
    println!(
        "Launched tool-owned child pid={} backed by {}",
        game.pid(),
        self_exe.display()
    );
    println!("Trusted binary hash: {}…", &game.expected_hash_hex()[..16]);
    for round in 1..=2 {
        std::thread::sleep(Duration::from_millis(150));
        match game.attest()? {
            AttestOutcome::Ok { pid } => {
                println!("  attest #{round}: OK — pid {pid} alive, binary hash matches")
            }
            AttestOutcome::Exited => println!("  attest #{round}: process exited"),
            AttestOutcome::BinaryTampered { .. } => {
                println!("  attest #{round}: BINARY TAMPERED — hash mismatch")
            }
        }
    }
    game.terminate();
    std::thread::sleep(Duration::from_millis(100));
    match game.attest()? {
        AttestOutcome::Exited => println!("  attest after terminate: process exited (expected)"),
        other => println!("  attest after terminate: {other:?}"),
    }

    // ------------------------------------------------------------------
    // 6. Heartbeat challenge/response with rolling HMAC (anti-replay).
    // ------------------------------------------------------------------
    section("6. Heartbeat challenge/response (rolling HMAC anti-replay)");
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(12345);
    let mut server = Server::new(DEMO_HEARTBEAT_KEY, seed);
    let mut client = Client::new(DEMO_HEARTBEAT_KEY);

    // Three honest rounds.
    let mut captured = None;
    for round in 1..=3 {
        let challenge = server.issue_challenge();
        let hb = client.respond(&challenge);
        if round == 1 {
            captured = Some(hb.clone()); // stash a valid heartbeat to replay later
        }
        println!(
            "  round {round}: counter={} -> {:?}",
            hb.counter,
            server.verify(&hb)
        );
    }

    // Replay attack: re-send round 1's already-accepted heartbeat.
    let replay = captured.expect("captured heartbeat");
    println!(
        "  replay of round 1 heartbeat -> {:?}  <- rejected",
        server.verify(&replay)
    );

    // Forgery attack: attacker without the shared key answers a fresh challenge.
    let challenge = server.issue_challenge();
    let mut attacker = Client::new(b"attacker-does-not-know-the-key");
    let forged_hb = attacker.respond(&challenge);
    let outcome = server.verify(&forged_hb);
    println!("  forged heartbeat (wrong key) -> {outcome:?}  <- rejected");
    assert_eq!(outcome, Outcome::RejectedBadMac);

    // ------------------------------------------------------------------
    // 7. Summary.
    // ------------------------------------------------------------------
    section("Summary");
    println!(
        "Integrity: {} tamper finding(s) detected against the signed manifest.",
        dirty.changes.len()
    );
    println!("Manifest signature protects the manifest from silent edits.");
    println!("Process attestation verified only the child this tool spawned.");
    println!("Heartbeat rejected both a replayed and a forged message.");
    println!("\nReminder: this is a defensive, educational userspace prototype only.");

    // Cleanup the sandbox we created.
    let _ = fs::remove_dir_all(sandbox.parent().unwrap_or(&sandbox));
    Ok(())
}

/// Create a fresh temp sandbox and copy the seed assets into it.
fn make_sandbox(seed_dir: &Path) -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("ranguard_sandbox_{nanos}"));
    let assets = base.join("assets");
    fs::create_dir_all(&assets)?;
    if seed_dir.exists() {
        copy_dir(seed_dir, &assets)?;
    } else {
        // Fallback seed so the demo still runs if assets/ is missing.
        write_file(&assets.join("config.cfg"), b"fov=90\nsensitivity=2.5\n")?;
        write_file(&assets.join("textures.pak"), &vec![0xABu8; 4096])?;
        write_file(&assets.join("scripts/init.lua"), b"-- game init\nprint('ok')\n")?;
    }
    Ok(assets)
}

/// Recursively copy a directory tree.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn write_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(path)?;
    f.write_all(contents)
}

/// Apply a deterministic tamper: modify + add + remove one file each.
fn tamper(sandbox: &Path) -> std::io::Result<()> {
    write_file(&sandbox.join("config.cfg"), b"fov=90\nsensitivity=2.5\nwallhack=true\n")?;
    write_file(&sandbox.join("cheat.dll"), b"\x00\x01injected-payload\x02\x03")?;
    let textures = sandbox.join("textures.pak");
    if textures.exists() {
        fs::remove_file(textures)?;
    }
    Ok(())
}
