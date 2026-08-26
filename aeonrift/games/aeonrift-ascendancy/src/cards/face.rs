//! The card-face bridge: a played card's picture.
//!
//! THE PICTURE SEAM. A [`crate::cards::card::Card`] already carries its champion's
//! DATA (stats + abilities) via the shared roster spec; this module ties that same
//! card to its finished painterly **face** — the framed PNG the card compositor
//! produced under `assets/cards/final-v2/`. The link is the champion's file **slug**:
//! the spec `content/champions/<slug>.ron` and the face `.../final-v2/<slug>.png`
//! share one stem, so *the card you play is the card you see*.
//!
//! Nothing here does image IO — it only resolves paths and file names, so it is cheap
//! to call from the headless replay gallery, the HTML board, and the Bevy client's
//! texture loader alike. The one test asserts every roster card has a real face on
//! disk (the played-card === pictured-card invariant, enforced).

use std::path::{Path, PathBuf};

/// The finished card-face directory (the compositor's `final-v2` output), resolved at
/// compile time relative to this crate. Every roster champion has a `<slug>.png` here.
pub const CARD_FACES_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/cards/final-v2");

/// The face image file name for a champion slug (e.g. `"ra-radiant-dawn"` =>
/// `"ra-radiant-dawn.png"`). Pure string math, no IO.
pub fn face_filename(slug: &str) -> String {
    format!("{slug}.png")
}

/// The absolute path to a card face inside `faces_dir` (the compositor output dir).
pub fn face_path_in(faces_dir: impl AsRef<Path>, slug: &str) -> PathBuf {
    faces_dir.as_ref().join(face_filename(slug))
}

/// The absolute path to a card face inside the committed [`CARD_FACES_DIR`].
pub fn default_face_path(slug: &str) -> PathBuf {
    face_path_in(CARD_FACES_DIR, slug)
}

/// Does a finished face exist on disk for this slug in the default faces dir?
pub fn face_exists(slug: &str) -> bool {
    default_face_path(slug).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::card_pool::CardPool;

    #[test]
    fn face_filename_is_the_slug_plus_png() {
        assert_eq!(face_filename("ra-radiant-dawn"), "ra-radiant-dawn.png");
        assert_eq!(face_filename("zeus"), "zeus.png");
    }

    #[test]
    fn every_roster_card_has_a_finished_face_on_disk() {
        // The played-card === pictured-card invariant: every champion the pool loads
        // as a playable card must have a finished painterly face in `final-v2`.
        let pool = CardPool::load_default().expect("roster pool loads");
        let mut missing: Vec<String> = Vec::new();
        for entry in pool.entries() {
            if !face_exists(&entry.slug) {
                missing.push(entry.slug.clone());
            }
        }
        assert!(
            missing.is_empty(),
            "roster cards missing a finished face in {}: {:?}",
            CARD_FACES_DIR,
            missing
        );
        // The finished set is the 47 shipped cards; guard against silent shrinkage.
        assert!(
            pool.entries().len() >= 47,
            "expected at least 47 playable roster cards, found {}",
            pool.entries().len()
        );
    }

    #[test]
    fn unknown_slug_has_no_face() {
        assert!(!face_exists("not-a-real-champion"));
    }
}
