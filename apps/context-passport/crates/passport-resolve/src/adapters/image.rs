//! Image conditioning: portable reference images + LoRA/embedding + style prompt.

use passport_core::{PassportAssets, PassportVersion};

use crate::adapter::{join, ProviderAdapter};
use crate::modality::Modality;
use crate::payload::ConditioningPayload;

pub struct ImageAdapter;

impl ProviderAdapter for ImageAdapter {
    fn build(
        &self,
        version: &PassportVersion,
        provider: &str,
        modality: Modality,
    ) -> ConditioningPayload {
        let mut p =
            ConditioningPayload::base(&version.passport_id, version.version, provider, modality);
        match &version.assets {
            PassportAssets::Character(c) => {
                p.reference_images = c.face_refs.clone();
                p.lora_ref = c.lora_ref.clone();
                p.style_prompt = join([c.style.as_deref(), c.wardrobe.as_deref()]);
            }
            PassportAssets::Brand(b) => {
                p.reference_images = [b.logo_ref.clone(), b.watermark_ref.clone()]
                    .into_iter()
                    .flatten()
                    .collect();
                p.style_prompt = join([b.typography.as_deref(), b.voice_tone.as_deref()]);
                if !b.palette.is_empty() {
                    p.extra.insert("palette".into(), b.palette.join(","));
                }
            }
            PassportAssets::Persona(persona) => {
                if let Some(platform) = &persona.platform {
                    p.extra.insert("platform".into(), platform.clone());
                }
                p.style_prompt = join([persona.audience.as_deref()]);
            }
        }
        p
    }
}
