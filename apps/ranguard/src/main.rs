//! Demo driver for the Vanguard-style Integrity Monitor.
//!
//! DEFENSIVE / USERSPACE / EDUCATIONAL. Everything here operates on a sandbox
//! this tool creates and owns. It does NOT install a kernel driver, does NOT
//! read other processes' memory, and does NOT do anti-debugging. See README.
//!
//! Usage:
//!   ranguard                          Run the full end-to-end demo.
//!   ranguard demo                     Same as above.
//!   ranguard snapshot <dir> [out]     Build a signed integrity manifest over a
//!                                      real directory you own and write it to
//!                                      `out` (default: `<dir>.ranguard.manifest`).
//!   ranguard verify <dir> <manifest>  Verify the manifest signature and re-scan
//!                                      `dir` for tampering against it.
//!   ranguard game-loop [ms]           Internal: the tool's own "game" child.
//!
//! `snapshot` and `verify` are the real, drivable tool: they expose the library's
//! integrity-monitoring primitives on a directory the operator explicitly hands
//! to the tool. The signing key is read from the `RANGUARD_KEY` environment
//! variable (never hardcoded) — the same key must be used to snapshot and verify.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ranguard::heartbeat::{Client, Outcome, Server};
use ranguard::manifest::Manifest;
use ranguard::process::{AttestOutcome, GameProcess};

// Clearly-labeled DEMO keys, used ONLY by the scripted `demo`. The real
// `snapshot`/`verify` commands never use these — they read a secret from the
// environment (see `load_signing_key`). A real tool would provision the key via
// a secret manager, never hardcode it (see README SAFETY section).
const DEMO_SIGNING_KEY: &[u8] = b"ranguard-demo-signing-key-not-a-real-secret";
const DEMO_HEARTBEAT_KEY: &[u8] = b"ranguard-demo-heartbeat-key-not-a-real-secret";

/// Environment variable holding the operator's manifest-signing secret.
const KEY_ENV: &str = "RANGUARD_KEY";

/// Process exit codes, kept explicit so `verify` is scriptable in CI.
const EXIT_OK: i32 = 0;
/// An integrity violation was detected (bad signature or tampered files), or a
/// runtime error occurred while processing a well-formed request.
const EXIT_VIOLATION: i32 = 1;
/// The invocation itself was invalid (bad args, missing key, missing path).
const EXIT_USAGE: i32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("demo");

    match cmd {
        "game-loop" => run_game_loop(&args),
        "demo" => {
            if let Err(e) = run_demo() {
                eprintln!("demo failed: {e}");
                std::process::exit(EXIT_VIOLATION);
            }
        }
        "snapshot" => std::process::exit(run_snapshot(&args)),
        "verify" => std::process::exit(run_verify(&args)),
        "help" | "-h" | "--help" => print_usage(&mut std::io::stdout()),
        other => {
            eprintln!("unknown command: {other}");
            print_usage(&mut std::io::stderr());
            std::process::exit(EXIT_USAGE);
        }
    }
}

fn print_usage(out: &mut impl Write) {
    let _ = writeln!(
        out,
        "ranguard — defensive, userspace integrity monitor\n\
         \n\
         usage:\n\
         \x20 ranguard [demo]                     run the scripted end-to-end demo\n\
         \x20 ranguard snapshot <dir> [out]       build & sign an integrity manifest over <dir>\n\
         \x20 ranguard verify   <dir> <manifest>  verify signature + re-scan <dir> for tampering\n\
         \n\
         `snapshot` and `verify` read the signing key from the {KEY_ENV} environment variable.\n\
         Example:\n\
         \x20 RANGUARD_KEY=$(head -c32 /dev/urandom | base64) ranguard snapshot ./game_assets\n\
         \x20 RANGUARD_KEY=... ranguard verify ./game_assets ./game_assets.ranguard.manifest"
    );
}

/// Read the operator's signing key from the environment. Fails fast with a clear
/// message if it is missing or empty — we never fall back to a hardcoded secret.
fn load_signing_key() -> Result<Vec<u8>, String> {
    match std::env::var(KEY_ENV) {
        Ok(k) if !k.is_empty() => Ok(k.into_bytes()),
        Ok(_) => Err(format!(
            "{KEY_ENV} is set but empty; provide a non-empty signing key"
        )),
        Err(_) => Err(format!(
            "{KEY_ENV} must be set to the manifest signing key (it protects the \
             manifest's HMAC signature; use the same value to snapshot and verify)"
        )),
    }
}

/// Default manifest path for a snapshotted directory: a sibling file named
/// `<dirname>.ranguard.manifest`, so the manifest lives next to what it protects.
fn default_manifest_path(dir: &Path) -> PathBuf {
    match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(name)) => {
            let mut file = name.to_os_string();
            file.push(".ranguard.manifest");
            parent.join(file)
        }
        _ => dir.join("ranguard.manifest"),
    }
}

/// `ranguard snapshot <dir> [out]` — fingerprint a real directory the operator
/// owns and write a signed manifest. Returns a process exit code.
fn run_snapshot(args: &[String]) -> i32 {
    let dir = match args.get(2) {
        Some(d) => PathBuf::from(d),
        None => {
            eprintln!("usage: ranguard snapshot <dir> [manifest-out]");
            return EXIT_USAGE;
        }
    };
    if !dir.is_dir() {
        eprintln!("error: {} is not a directory", dir.display());
        return EXIT_USAGE;
    }
    let key = match load_signing_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let manifest = match Manifest::build(&dir, &key) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: failed to build manifest for {}: {e}", dir.display());
            return EXIT_VIOLATION;
        }
    };
    let out = args
        .get(3)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_manifest_path(&dir));
    if let Err(e) = fs::write(&out, manifest.serialize()) {
        eprintln!("error: failed to write manifest to {}: {e}", out.display());
        return EXIT_VIOLATION;
    }

    println!("Signed integrity manifest written to {}", out.display());
    println!("Root: {}", dir.display());
    println!("Hashed {} file(s) with SHA-256:", manifest.entries.len());
    for e in &manifest.entries {
        println!(
            "  {:<40} {:>10} bytes  {}…",
            e.rel_path,
            e.size,
            &e.sha256_hex[..16.min(e.sha256_hex.len())]
        );
    }
    println!("HMAC-SHA256 signature: {}", manifest.mac_hex);
    EXIT_OK
}

/// `ranguard verify <dir> <manifest>` — verify the manifest's own signature, then
/// re-scan `dir` and report tampering. Exit code: 0 = clean & signed, 1 = any
/// violation (bad signature or file changes), 2 = usage/setup error.
fn run_verify(args: &[String]) -> i32 {
    let (dir, manifest_path) = match (args.get(2), args.get(3)) {
        (Some(d), Some(m)) => (PathBuf::from(d), PathBuf::from(m)),
        _ => {
            eprintln!("usage: ranguard verify <dir> <manifest>");
            return EXIT_USAGE;
        }
    };
    if !dir.is_dir() {
        eprintln!("error: {} is not a directory", dir.display());
        return EXIT_USAGE;
    }
    let key = match load_signing_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };

    let text = match fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "error: cannot read manifest {}: {e}",
                manifest_path.display()
            );
            return EXIT_USAGE;
        }
    };
    let manifest = match Manifest::deserialize(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: malformed manifest {}: {e}", manifest_path.display());
            return EXIT_VIOLATION;
        }
    };

    // 1. Does the manifest itself still carry a valid signature under our key?
    let sig_ok = manifest.verify_signature(&key);
    println!("Manifest: {}", manifest_path.display());
    println!("Root scanned: {}", dir.display());
    println!(
        "Signature (HMAC-SHA256): {}",
        if sig_ok {
            "VALID"
        } else {
            "INVALID — manifest was edited or the wrong key was used"
        }
    );

    // 2. Re-scan the live directory against the trusted entries.
    let report = match manifest.scan(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to scan {}: {e}", dir.display());
            return EXIT_VIOLATION;
        }
    };
    print!("{}", report.render());

    if sig_ok && report.is_clean() {
        println!("RESULT: OK — signature valid and all files match the manifest.");
        EXIT_OK
    } else {
        println!("RESULT: FAIL — integrity violation detected.");
        EXIT_VIOLATION
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
    println!(
        "Hashed {} asset file(s) with SHA-256:",
        manifest.entries.len()
    );
    for e in &manifest.entries {
        println!(
            "  {:<20} {} bytes  {}…",
            e.rel_path,
            e.size,
            &e.sha256_hex[..16]
        );
    }
    println!(
        "Manifest signed (HMAC-SHA256): {}…",
        &manifest.mac_hex[..16]
    );
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
    let mut server = Server::new_secure(DEMO_HEARTBEAT_KEY)?;
    let mut client = Client::new(DEMO_HEARTBEAT_KEY);

    // Three honest rounds. The client advances its rolling chain only after the
    // server confirms acceptance (commit), keeping both chains in lockstep.
    let mut captured = None;
    for round in 1..=3 {
        let challenge = server.issue_challenge();
        let hb = client.respond(&challenge);
        if round == 1 {
            captured = Some(hb.clone()); // stash a valid heartbeat to replay later
        }
        let outcome = server.verify(&hb);
        if outcome == Outcome::Accepted {
            client.commit(&hb);
        }
        println!("  round {round}: counter={} -> {outcome:?}", hb.counter);
    }

    // Replay attack: re-send round 1's already-accepted heartbeat.
    let replay = captured.expect("captured heartbeat");
    println!(
        "  replay of round 1 heartbeat -> {:?}  <- rejected",
        server.verify(&replay)
    );

    // Forgery attack: attacker without the shared key answers a fresh challenge.
    let challenge = server.issue_challenge();
    let attacker = Client::new(b"attacker-does-not-know-the-key");
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
        write_file(
            &assets.join("scripts/init.lua"),
            b"-- game init\nprint('ok')\n",
        )?;
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
    write_file(
        &sandbox.join("config.cfg"),
        b"fov=90\nsensitivity=2.5\nwallhack=true\n",
    )?;
    write_file(
        &sandbox.join("cheat.dll"),
        b"\x00\x01injected-payload\x02\x03",
    )?;
    let textures = sandbox.join("textures.pak");
    if textures.exists() {
        fs::remove_file(textures)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_manifest_path_is_sibling_of_dir() {
        let got = default_manifest_path(Path::new("/tmp/game_assets"));
        assert_eq!(got, PathBuf::from("/tmp/game_assets.ranguard.manifest"));
    }

    #[test]
    fn default_manifest_path_relative_dir() {
        let got = default_manifest_path(Path::new("assets"));
        assert_eq!(got, PathBuf::from("assets.ranguard.manifest"));
    }

    #[test]
    fn default_manifest_path_root_fallback() {
        // A path with no file name falls back to a manifest inside the dir.
        let got = default_manifest_path(Path::new("/"));
        assert_eq!(got, PathBuf::from("/ranguard.manifest"));
    }

    #[test]
    fn exit_codes_are_distinct() {
        assert_ne!(EXIT_OK, EXIT_VIOLATION);
        assert_ne!(EXIT_OK, EXIT_USAGE);
        assert_ne!(EXIT_VIOLATION, EXIT_USAGE);
    }
}
