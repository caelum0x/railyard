//! # aeonrift-audio-engine
//!
//! ROLE: SFX bus, music, and sound loading built on **kira** — a real, engine
//! -agnostic game audio library (no Bevy). Because kira never pulls Bevy into the
//! dependency graph it is a DEFAULT dependency; the "default build must not pull
//! Bevy 0.19" constraint is unaffected.
//!
//! ## What's here
//! - [`AudioEngine`] — wraps a kira mixer; loads/plays named sfx + music at a master
//!   volume. Generic over the kira backend, so production uses the real device
//!   backend and tests use kira's device-free `MockBackend` on the same code path.
//! - [`SoundLibrary`] + [`pcm_mono`] / [`pcm_stereo`] — build sounds from raw PCM
//!   (deterministic, decoder-free) or decode WAV/OGG bytes via kira's symphonia.
//! - [`AudioError`] — a single error type for backend / decode / play / lookup.

pub mod error;
pub mod manager;
pub mod sound;

pub use error::AudioError;
pub use manager::AudioEngine;
pub use sound::{pcm_mono, pcm_stereo, SoundLibrary};

// Re-export the kira handle types callers touch, so downstream crates need not add
// a direct kira dependency just to name a running sound.
pub use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};

#[cfg(test)]
mod tests {
    use super::*;
    use kira::backend::mock::MockBackend;
    use kira::AudioManagerSettings;

    /// A device-free engine for tests (kira's mock backend — no audio hardware).
    fn mock_engine() -> AudioEngine<MockBackend> {
        AudioEngine::<MockBackend>::with_settings(AudioManagerSettings::default())
            .expect("mock backend always initializes")
    }

    #[test]
    fn pcm_sound_has_expected_length_and_rate() {
        // 0.5s of silence at 8 kHz mono = 4000 frames.
        let data = pcm_mono(8000, &vec![0.0_f32; 4000]);
        assert_eq!(data.sample_rate, 8000);
        assert_eq!(data.num_frames(), 4000);
    }

    #[test]
    fn library_registers_and_looks_up_in_name_order() {
        let mut lib = SoundLibrary::new();
        assert!(lib.is_empty());
        lib.insert("hit", pcm_mono(8000, &[0.1, -0.1, 0.1]));
        lib.insert("cast", pcm_stereo(8000, &[(0.2, 0.2), (-0.2, -0.2)]));
        assert_eq!(lib.len(), 2);
        assert!(lib.contains("hit"));
        assert!(!lib.contains("missing"));
        // BTreeMap => stable sorted iteration.
        assert_eq!(lib.names().collect::<Vec<_>>(), vec!["cast", "hit"]);
    }

    #[test]
    fn plays_a_registered_sfx_through_the_mock_mixer() {
        let mut engine = mock_engine();
        engine.register_sfx("hit", pcm_mono(8000, &vec![0.25_f32; 16]));
        // Playing a loaded sfx must succeed and return a live handle.
        let handle = engine.play_sfx("hit").expect("sfx plays");
        // The handle is a real kira StaticSoundHandle (proves the mixer accepted it).
        let _ = &handle;
    }

    #[test]
    fn playing_an_unloaded_sound_is_a_clean_error() {
        let mut engine = mock_engine();
        let err = engine.play_sfx("nope").unwrap_err();
        assert_eq!(err, AudioError::NotLoaded("nope".into()));
    }

    #[test]
    fn looping_music_plays() {
        let mut engine = mock_engine();
        engine.register_music("theme", pcm_mono(8000, &vec![0.1_f32; 64]));
        engine.play_music("theme", true).expect("looping music plays");
    }

    #[test]
    fn master_volume_roundtrips() {
        let mut engine = mock_engine();
        assert_eq!(engine.master_volume_db(), 0.0);
        engine.set_master_volume_db(-6.0);
        assert_eq!(engine.master_volume_db(), -6.0);
        // Volume is applied at play time (no panic / error with a non-zero master).
        engine.register_sfx("s", pcm_mono(8000, &[0.0, 0.0]));
        engine.play_sfx("s").expect("plays at reduced volume");
    }

    #[test]
    fn decodes_wav_bytes() {
        // A tiny valid 16-bit PCM mono WAV (8 samples @ 8 kHz), built in-memory, is
        // decoded by kira's symphonia path — proving the encoded-load route works.
        let wav = tiny_wav_8_samples();
        let mut lib = SoundLibrary::new();
        let frames = lib.load_encoded("blip", wav).expect("wav decodes");
        assert_eq!(frames, 8, "decoded frame count matches the WAV payload");
        assert!(lib.contains("blip"));
    }

    /// Build a minimal canonical WAV: RIFF + fmt (PCM16 mono 8 kHz) + 8 sample data.
    fn tiny_wav_8_samples() -> Vec<u8> {
        let sample_rate: u32 = 8000;
        let channels: u16 = 1;
        let bits: u16 = 16;
        let samples: [i16; 8] = [0, 4000, 8000, 4000, 0, -4000, -8000, -4000];
        let data_len = (samples.len() * 2) as u32;
        let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
        let block_align = channels * (bits / 8);

        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVE");
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&sample_rate.to_le_bytes());
        w.extend_from_slice(&byte_rate.to_le_bytes());
        w.extend_from_slice(&block_align.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            w.extend_from_slice(&s.to_le_bytes());
        }
        w
    }
}
