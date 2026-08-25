// `scb` is the Riot static-mesh (.scb) loader (league_file/league_to_lol). Gated OFF
// by default — Aeonrift feeds particles ORIGINAL glTF/Bevy-native meshes instead.
#[cfg(feature = "riot-assets")]
pub mod scb;
pub mod vfx;
