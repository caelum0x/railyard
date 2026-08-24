//! Concrete per-modality adapters. Each maps a locked version's typed assets
//! onto the provider-neutral [`ConditioningPayload`] for one modality, across all
//! three passport kinds.

mod image;
mod text;
mod video;
mod voice;

pub use image::ImageAdapter;
pub use text::TextAdapter;
pub use video::VideoAdapter;
pub use voice::VoiceAdapter;
