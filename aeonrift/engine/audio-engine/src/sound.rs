//! Loading and holding decoded sounds.
//!
//! A [`SoundLibrary`] is a named cache of [`StaticSoundData`] (audio loaded fully
//! into memory). Sounds enter it two ways:
//! - [`pcm_mono`] / [`pcm_stereo`] — build directly from raw PCM samples (engine
//!   -agnostic, no decoder, fully deterministic — good for procedural sfx + tests),
//! - [`SoundLibrary::load_encoded`] — decode WAV/OGG bytes via kira's symphonia.
//!
//! `StaticSoundData` is cheap to clone (its frames are `Arc`-shared), so playing a
//! sound clones the cached entry and hands it to the mixer.

use std::collections::BTreeMap;
use std::io::Cursor;

use kira::sound::static_sound::StaticSoundData;
use kira::Frame;

use crate::error::AudioError;

/// Build a mono [`StaticSoundData`] from raw `f32` samples in `[-1.0, 1.0]`.
pub fn pcm_mono(sample_rate: u32, samples: &[f32]) -> StaticSoundData {
    let frames: Vec<Frame> = samples.iter().map(|&s| Frame::new(s, s)).collect();
    from_frames(sample_rate, frames)
}

/// Build a stereo [`StaticSoundData`] from raw `(left, right)` sample pairs.
pub fn pcm_stereo(sample_rate: u32, frames: &[(f32, f32)]) -> StaticSoundData {
    let frames: Vec<Frame> = frames.iter().map(|&(l, r)| Frame::new(l, r)).collect();
    from_frames(sample_rate, frames)
}

/// Assemble a [`StaticSoundData`] from decoded frames (shared helper).
fn from_frames(sample_rate: u32, frames: Vec<Frame>) -> StaticSoundData {
    StaticSoundData {
        sample_rate: sample_rate.max(1),
        frames: frames.into(),
        settings: Default::default(),
        slice: None,
    }
}

/// A named, in-memory cache of loaded sounds, iterated in stable name order.
#[derive(Clone, Default)]
pub struct SoundLibrary {
    sounds: BTreeMap<String, StaticSoundData>,
}

impl SoundLibrary {
    /// An empty library.
    pub fn new() -> Self {
        SoundLibrary {
            sounds: BTreeMap::new(),
        }
    }

    /// Insert (or replace) a sound under `name`.
    pub fn insert(&mut self, name: impl Into<String>, data: StaticSoundData) {
        self.sounds.insert(name.into(), data);
    }

    /// Decode encoded audio (WAV/OGG) bytes and register them under `name`.
    /// Returns the decoded duration in frames for callers that want to verify it.
    pub fn load_encoded(
        &mut self,
        name: impl Into<String>,
        bytes: impl AsRef<[u8]> + Send + Sync + 'static,
    ) -> Result<usize, AudioError> {
        let data = StaticSoundData::from_cursor(Cursor::new(bytes))
            .map_err(|e| AudioError::Decode(e.to_string()))?;
        let frames = data.num_frames();
        self.sounds.insert(name.into(), data);
        Ok(frames)
    }

    /// Look up a loaded sound by name.
    pub fn get(&self, name: &str) -> Option<&StaticSoundData> {
        self.sounds.get(name)
    }

    /// True if `name` is loaded.
    pub fn contains(&self, name: &str) -> bool {
        self.sounds.contains_key(name)
    }

    /// Number of loaded sounds.
    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    /// True if nothing is loaded.
    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }

    /// All loaded names, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.sounds.keys().map(|s| s.as_str())
    }
}
