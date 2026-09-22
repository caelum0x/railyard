//! # aeonrift-content-lint
//!
//! ROLE: Schema/closed-vocab validator for content/**.ron (unknown effect/targeting = red).
//!
//! MIGRATE: moon-lol content/_schema validation + moba/champions loader checks.
//!
//! DEPENDS_ON (workspace path deps):
//!   - `aeonrift-ability-dsl` (imported as `champions`)
//!
//! STATUS: runnable. This is the batch content gate described in
//! AEONRIFT-MASTER-PLAN.md. It wraps the ability-DSL's single load+validate gate
//! ([`champions::load_file`]) over every champion spec in a content directory,
//! producing an aggregate, CI-consumable [`LintReport`]. Where the ability-DSL
//! integration tests *assert* the shipped roster is green, this crate is the
//! standalone linter a content author (or CI job) runs against an arbitrary
//! content tree to get a per-file pass/fail report plus a non-zero exit on any
//! failure.
//!
//! Only champion specs are linted today because `ChampionSpec` is the one content
//! shape with a typed, closed-vocabulary DSL. As item/rune/status DSLs land in
//! `aeonrift-ability-dsl`, add their loaders here behind the same report type.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use champions::load_file;

/// File stems that live in the champions content dir but are NOT `ChampionSpec`s
/// and must therefore be skipped by the linter (sidecar metadata). Kept in sync
/// with the exclusions in the ability-DSL roster tests.
const NON_SPEC_STEMS: &[&str] = &["roster_meta"];

/// The outcome of linting a single content file.
#[derive(Debug, Clone)]
pub struct FileOutcome {
    /// The file stem (e.g. `"achilles"`), used as a stable, human-facing id.
    pub slug: String,
    /// The path that was linted.
    pub path: PathBuf,
    /// `None` when the file loaded + validated; otherwise the failure reason
    /// (the ability-DSL `SpecError`'s `Display` message).
    pub error: Option<String>,
}

impl FileOutcome {
    /// Whether this file passed load + validation.
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// The aggregate result of linting a content directory.
#[derive(Debug, Clone, Default)]
pub struct LintReport {
    /// One entry per linted file, in stable (sorted-path) order.
    pub outcomes: Vec<FileOutcome>,
}

impl LintReport {
    /// Number of files that loaded + validated.
    pub fn passed(&self) -> usize {
        self.outcomes.iter().filter(|o| o.is_ok()).count()
    }

    /// Number of files that failed to load or validate.
    pub fn failed(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.is_ok()).count()
    }

    /// Total number of files linted (excludes skipped sidecars).
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    /// True when every linted file passed. An empty report passes vacuously.
    pub fn all_passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.is_ok())
    }

    /// The failing outcomes only, for reporting.
    pub fn failures(&self) -> impl Iterator<Item = &FileOutcome> {
        self.outcomes.iter().filter(|o| !o.is_ok())
    }
}

/// True if `path` is a `.ron` champion spec (i.e. not a skipped sidecar stem).
fn is_champion_spec(path: &Path) -> bool {
    let is_ron = path.extension().map(|x| x == "ron").unwrap_or(false);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    is_ron && !NON_SPEC_STEMS.contains(&stem)
}

/// Collect, in stable sorted order, every champion-spec `.ron` file directly
/// inside `dir` (non-recursive; sidecar stems in [`NON_SPEC_STEMS`] excluded).
pub fn champion_spec_paths(dir: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_champion_spec(p))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Lint a single champion-spec file through the ability-DSL load+validate gate.
pub fn lint_champion_file(path: impl AsRef<Path>) -> FileOutcome {
    let path = path.as_ref().to_path_buf();
    let slug = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("<unknown>")
        .to_string();
    let error = load_file(&path).err().map(|e| e.to_string());
    FileOutcome { slug, path, error }
}

/// Lint every champion spec in `dir`, returning an aggregate [`LintReport`].
///
/// Returns an error only if the directory itself cannot be read; per-file
/// parse/validate failures are captured as failing [`FileOutcome`]s in the
/// report (so one bad champion never hides the rest).
pub fn lint_champions_dir(dir: impl AsRef<Path>) -> io::Result<LintReport> {
    let outcomes = champion_spec_paths(dir)?
        .iter()
        .map(lint_champion_file)
        .collect();
    Ok(LintReport { outcomes })
}

/// The workspace's canonical champions content directory, resolved relative to
/// this crate so the binary works regardless of the caller's cwd.
pub fn default_champions_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/champions")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, valid champion spec (self-cast heal — needs no range).
    const GOOD_SPEC: &str = r#"ChampionSpec(
        name: "Testliss",
        base: BaseStats(hp: 100, ad: 10, armor: 5, attack_speed: 1.0, move_speed: 5.0, range: 1.0),
        abilities: [
            AbilitySpec(name: "Mend", cooldown: 5, cost: None, targeting: SelfCast, effects: [ Heal(amount: 10) ]),
        ],
    )"#;

    /// Names an effect (`Teleport`) OUTSIDE the closed vocabulary — must be rejected
    /// at parse time by RON's unknown-variant check.
    const BAD_SPEC: &str = r#"ChampionSpec(
        name: "Bad",
        base: BaseStats(hp: 100, ad: 10, armor: 5, attack_speed: 1.0, move_speed: 5.0, range: 1.0),
        abilities: [
            AbilitySpec(name: "Blink", cooldown: 5, targeting: SelfCast, effects: [ Teleport(distance: 3.0) ]),
        ],
    )"#;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aeonrift-content-lint-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn shipped_roster_all_passes() {
        let report =
            lint_champions_dir(default_champions_dir()).expect("champions content dir must exist");
        assert!(report.total() > 0, "expected at least one champion spec");
        assert!(
            report.all_passed(),
            "shipped roster must lint clean, failures: {:?}",
            report.failures().collect::<Vec<_>>()
        );
        assert_eq!(report.failed(), 0);
        assert_eq!(report.passed(), report.total());
    }

    #[test]
    fn sidecar_metadata_is_skipped() {
        // The RosterMeta sidecar is NOT a ChampionSpec; it must never be linted.
        let report = lint_champions_dir(default_champions_dir()).unwrap();
        assert!(
            !report.outcomes.iter().any(|o| o.slug == "roster_meta"),
            "roster_meta sidecar must be skipped, not linted"
        );
    }

    #[test]
    fn good_spec_passes_bad_spec_fails() {
        let dir = scratch_dir("mixed");
        fs::write(dir.join("good.ron"), GOOD_SPEC).unwrap();
        fs::write(dir.join("bad.ron"), BAD_SPEC).unwrap();
        // A sidecar that must be ignored even in an arbitrary tree.
        fs::write(dir.join("roster_meta.ron"), "RosterMeta()").unwrap();

        let report = lint_champions_dir(&dir).unwrap();
        assert_eq!(
            report.total(),
            2,
            "sidecar should be excluded from the count"
        );
        assert!(!report.all_passed());
        assert_eq!(report.passed(), 1);
        assert_eq!(report.failed(), 1);

        let bad = report.outcomes.iter().find(|o| o.slug == "bad").unwrap();
        let msg = bad.error.as_deref().unwrap();
        assert!(
            msg.contains("unknown variant") || msg.to_lowercase().contains("teleport"),
            "closed-vocab violation must be reported clearly, got: {msg}"
        );
        assert!(report
            .outcomes
            .iter()
            .find(|o| o.slug == "good")
            .unwrap()
            .is_ok());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_dir_passes_vacuously() {
        let dir = scratch_dir("empty");
        let report = lint_champions_dir(&dir).unwrap();
        assert_eq!(report.total(), 0);
        assert!(report.all_passed());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_is_an_error() {
        let missing = std::env::temp_dir().join("aeonrift-content-lint-does-not-exist-xyz");
        assert!(lint_champions_dir(&missing).is_err());
    }
}
