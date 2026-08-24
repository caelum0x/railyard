//! Voice conditioning. Per the provider research, a voice identity is a
//! non-portable per-provider clone token (`voice_id`); the capability layer flags
//! `needs_artifact` when it is absent.

use passport_core::{PassportAssets, PassportVersion};

use crate::adapter::ProviderAdapter;
use crate::modality::Modality;
use crate::payload::ConditioningPayload;

pub struct VoiceAdapter;

impl ProviderAdapter for VoiceAdapter {
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
                p.voice_id = c.voice_id.clone();
                if let Some(personality) = &c.personality {
                    p.text_directives.push(format!("tone: {personality}"));
                }
            }
            PassportAssets::Brand(b) => {
                if let Some(v) = &b.voice_tone {
                    p.text_directives.push(format!("brand voice: {v}"));
                }
            }
            PassportAssets::Persona(persona) => {
                if let Some(lang) = &persona.language {
                    p.extra.insert("language".into(), lang.clone());
                }
            }
        }
        p
    }
}
