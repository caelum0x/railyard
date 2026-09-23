//! Text/persona conditioning: fully portable directives (persona, brand voice, hooks).

use passport_core::{PassportAssets, PassportVersion};

use crate::adapter::ProviderAdapter;
use crate::modality::Modality;
use crate::payload::ConditioningPayload;

pub struct TextAdapter;

impl ProviderAdapter for TextAdapter {
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
                if let Some(personality) = &c.personality {
                    p.text_directives.push(format!("persona: {personality}"));
                }
                if let Some(style) = &c.style {
                    p.text_directives.push(format!("style: {style}"));
                }
            }
            PassportAssets::Brand(b) => {
                if let Some(v) = &b.voice_tone {
                    p.text_directives.push(format!("brand voice: {v}"));
                }
            }
            PassportAssets::Persona(persona) => {
                if let Some(audience) = &persona.audience {
                    p.text_directives.push(format!("audience: {audience}"));
                }
                if let Some(lang) = &persona.language {
                    p.extra.insert("language".into(), lang.clone());
                }
                for hook in &persona.hooks {
                    p.text_directives.push(format!("hook: {hook}"));
                }
            }
            PassportAssets::Voice(v) => {
                if let Some(delivery) = &v.delivery {
                    p.text_directives.push(format!("delivery: {delivery}"));
                }
                if let Some(lang) = &v.language {
                    p.extra.insert("language".into(), lang.clone());
                }
            }
            PassportAssets::Style(s) => {
                if let Some(descriptor) = &s.descriptor {
                    p.text_directives.push(format!("style: {descriptor}"));
                }
                if let Some(medium) = &s.medium {
                    p.text_directives.push(format!("medium: {medium}"));
                }
            }
        }
        p
    }
}
