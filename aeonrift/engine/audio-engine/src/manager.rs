//! The audio manager: a thin, real wrapper over kira's mixer.
//!
//! [`AudioEngine`] owns a kira [`AudioManager`] plus two [`SoundLibrary`] caches
//! (sfx + music) and a master volume. It is generic over the kira [`Backend`] so
//! production uses the real device backend ([`DefaultBackend`]) while tests use the
//! device-free `MockBackend` — the exact same code path either way.

use std::fmt::Debug;

use kira::backend::{Backend, DefaultBackend};
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::{AudioManager, AudioManagerSettings, Decibels};

use crate::error::AudioError;
use crate::sound::SoundLibrary;

/// A game audio manager over a kira mixer.
pub struct AudioEngine<B: Backend = DefaultBackend> {
    manager: AudioManager<B>,
    sfx: SoundLibrary,
    music: SoundLibrary,
    master: Decibels,
}

impl AudioEngine<DefaultBackend> {
    /// Open the default system audio device and build an engine on it. Use this in
    /// the shipping client. Fails if no audio device is available.
    pub fn new() -> Result<Self, AudioError> {
        Self::with_settings(AudioManagerSettings::default())
    }
}

impl<B: Backend> AudioEngine<B>
where
    B::Error: Debug,
{
    /// Build an engine from explicit kira settings (backend chosen by `B`). Tests
    /// pass `AudioManagerSettings::<MockBackend>::default()` for a device-free mixer.
    pub fn with_settings(settings: AudioManagerSettings<B>) -> Result<Self, AudioError> {
        let manager =
            AudioManager::<B>::new(settings).map_err(|e| AudioError::Backend(format!("{e:?}")))?;
        Ok(AudioEngine {
            manager,
            sfx: SoundLibrary::new(),
            music: SoundLibrary::new(),
            master: Decibels::IDENTITY,
        })
    }

    /// Mutable access to the sfx library (register / decode sounds).
    pub fn sfx_mut(&mut self) -> &mut SoundLibrary {
        &mut self.sfx
    }

    /// Mutable access to the music library.
    pub fn music_mut(&mut self) -> &mut SoundLibrary {
        &mut self.music
    }

    /// Register a ready [`StaticSoundData`] as a named sfx.
    pub fn register_sfx(&mut self, name: impl Into<String>, data: StaticSoundData) {
        self.sfx.insert(name, data);
    }

    /// Register a ready [`StaticSoundData`] as named music.
    pub fn register_music(&mut self, name: impl Into<String>, data: StaticSoundData) {
        self.music.insert(name, data);
    }

    /// Set the master output volume in decibels (0 dB = unchanged, negative =
    /// quieter). Applied to every subsequently played sound.
    pub fn set_master_volume_db(&mut self, db: f32) {
        self.master = Decibels(db);
    }

    /// The current master volume in decibels.
    pub fn master_volume_db(&self) -> f32 {
        self.master.0
    }

    /// Play a one-shot sfx by name at the current master volume. Returns a handle
    /// the caller can use to stop / modulate the running sound.
    pub fn play_sfx(&mut self, name: &str) -> Result<StaticSoundHandle, AudioError> {
        let data = self
            .sfx
            .get(name)
            .ok_or_else(|| AudioError::NotLoaded(name.to_string()))?
            .volume(self.master);
        self.manager
            .play(data)
            .map_err(|e| AudioError::Play(format!("{e:?}")))
    }

    /// Play music by name at the current master volume. When `looping`, the whole
    /// track loops seamlessly until its handle is stopped.
    pub fn play_music(
        &mut self,
        name: &str,
        looping: bool,
    ) -> Result<StaticSoundHandle, AudioError> {
        let base = self
            .music
            .get(name)
            .ok_or_else(|| AudioError::NotLoaded(name.to_string()))?
            .volume(self.master);
        let data = if looping { base.loop_region(..) } else { base };
        self.manager
            .play(data)
            .map_err(|e| AudioError::Play(format!("{e:?}")))
    }

    /// Escape hatch: the underlying kira manager (advanced mixer control).
    pub fn manager_mut(&mut self) -> &mut AudioManager<B> {
        &mut self.manager
    }
}
