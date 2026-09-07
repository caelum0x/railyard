#![expect(clippy::unwrap_used, clippy::print_stderr, clippy::panic)]

use std::path::Path;

const TS_VERSION: &str = "5.7.3";
const TS_BLAKE3: &str = "f4c7abd9363389427c082edf3056f17ac724a8f046d7266d44d456fede5692a9";

const SES_VERSION: &str = "1.14.0";
const SES_BLAKE3: &str = "8620364f19ff2f35689ae8600ecbdb248aff04b33171bf8821b83eff650c98d4";

const TS_SOURCE_ENV: &str = "TENSORZERO_TYPESCRIPT_JS_PATH";
const SES_SOURCE_ENV: &str = "TENSORZERO_SES_JS_PATH";

fn read_verified(path: &Path, expected_blake3: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (blake3::hash(&bytes).to_hex().as_str() == expected_blake3).then_some(bytes)
}

/// Cargo creates a different `OUT_DIR` for normal builds, tests, Clippy, and
/// feature sets. Reuse a pinned asset from any sibling `ts-executor-pool-*`
/// output directory before requiring network access again.
fn find_sibling_cache(destination: &Path, expected_blake3: &str) -> Option<(Vec<u8>, String)> {
    let file_name = destination.file_name()?;
    let build_dir = destination.parent()?.parent()?.parent()?;
    for entry in std::fs::read_dir(build_dir).ok()?.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("ts-executor-pool-")
        {
            continue;
        }
        let candidate = entry.path().join("out").join(file_name);
        if candidate == destination {
            continue;
        }
        if let Some(bytes) = read_verified(&candidate, expected_blake3) {
            return Some((bytes, candidate.display().to_string()));
        }
    }
    None
}

fn stage_pinned_asset(
    destination: &str,
    source_env: &str,
    url: &str,
    expected_blake3: &str,
    label: &str,
) {
    println!("cargo:rerun-if-env-changed={source_env}");
    let destination = Path::new(destination);
    if destination.exists() {
        return;
    }

    let bytes = if let Ok(source) = std::env::var(source_env) {
        eprintln!("Loading {label} from {source_env}={source}...");
        std::fs::read(&source)
            .unwrap_or_else(|error| panic!("Failed to read {label} from {source}: {error}"))
    } else if let Some((bytes, source)) = find_sibling_cache(destination, expected_blake3) {
        eprintln!("Reusing hash-verified {label} from {source}...");
        bytes
    } else {
        eprintln!("Downloading {label} from {url}...");
        let mut body = ureq::get(url)
            .call()
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to download {label} from {url}: {error}. For an offline build, set {source_env} to a local copy"
                )
            })
            .into_body();
        body.read_to_vec()
            .unwrap_or_else(|error| panic!("Failed to read {label} from {url}: {error}"))
    };

    let actual_hash = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(
        actual_hash, expected_blake3,
        "BLAKE3 mismatch for {label}: expected {expected_blake3}, got {actual_hash}"
    );
    std::fs::write(destination, bytes).unwrap();
    eprintln!(
        "Staged {label} ({} bytes, blake3: {expected_blake3})",
        std::fs::metadata(destination).unwrap().len()
    );
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();

    // -----------------------------------------------------------------------
    // TypeScript checker snapshot (unchanged)
    // -----------------------------------------------------------------------

    // Include the version in the filename so bumping TS_VERSION invalidates the cache.
    let ts_path = format!("{out_dir}/typescript-{TS_VERSION}.js");
    let checker_js = include_str!("src/js/ts_checker.js");
    // Include hashes of both JS inputs in the snapshot filename so it is
    // regenerated whenever either changes.
    let checker_hash = &blake3::hash(checker_js.as_bytes()).to_hex()[..16];
    let snapshot_path = format!("{out_dir}/TS_CHECKER_SNAPSHOT_{TS_BLAKE3}_{checker_hash}.bin");

    // Step 1: Stage typescript.js from an explicit offline source or download
    // it when the current Cargo output directory does not already contain it.
    let ts_url = format!("https://unpkg.com/typescript@{TS_VERSION}/lib/typescript.js");
    stage_pinned_asset(
        &ts_path,
        TS_SOURCE_ENV,
        &ts_url,
        TS_BLAKE3,
        &format!("typescript.js v{TS_VERSION}"),
    );

    // Step 2: Create V8 snapshot with TS compiler + checker helper preloaded
    if !Path::new(&snapshot_path).exists() {
        eprintln!("Creating TypeScript checker V8 snapshot...");
        let ts_js = std::fs::read_to_string(&ts_path).unwrap();

        let mut runtime =
            deno_core::JsRuntimeForSnapshot::new(deno_core::RuntimeOptions::default());
        runtime
            .execute_script("<typescript.js>", ts_js)
            .unwrap_or_else(|e| panic!("Failed to evaluate typescript.js: {e}"));
        runtime
            .execute_script("<ts_checker.js>", checker_js.to_string())
            .unwrap_or_else(|e| panic!("Failed to evaluate ts_checker.js: {e}"));

        let snapshot = runtime.snapshot();
        std::fs::write(&snapshot_path, &*snapshot).unwrap();
        eprintln!("Snapshot created ({} bytes)", snapshot.len());
    }

    // Tell downstream code where to find the snapshot.
    println!("cargo:rustc-env=TS_CHECKER_SNAPSHOT_PATH={snapshot_path}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/js/ts_checker.js");

    // -----------------------------------------------------------------------
    // SES (Secure ECMAScript) — downloaded and embedded at compile time
    // -----------------------------------------------------------------------

    // Step 3: Stage ses.umd.min.js (cached by version and content hash)
    let ses_path = format!("{out_dir}/ses-{SES_VERSION}.umd.min.js");
    let ses_url = format!("https://unpkg.com/ses@{SES_VERSION}/dist/ses.umd.min.js");
    stage_pinned_asset(
        &ses_path,
        SES_SOURCE_ENV,
        &ses_url,
        SES_BLAKE3,
        &format!("ses.umd.min.js v{SES_VERSION}"),
    );

    // Expose the path so runtime.rs can include_str! it at compile time.
    println!("cargo:rustc-env=SES_JS_PATH={ses_path}");
    println!("cargo:rerun-if-changed=src/js/ses_init.js");
}
