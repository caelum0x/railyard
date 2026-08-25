use bevy::app::Plugin;
// Aeonrift original-IP champion roster. Every module here is an original champion
// composed entirely from `lol_core`'s closed action/effect/buff vocabulary — ZERO
// Riot assets. Each champion is one Bevy `Plugin`; `PluginChampions` registers the
// full roster, and any champion can also be added individually via
// `app.add_plugins(champion::PluginChampionName)`.

// Eclipse Warden — the seed original champion.
pub mod erebos;

// ── Graduated Aeonrift originals (folded in from moon-lol graduate/champions) ──
pub mod dregmaw;
pub mod kaeza;
pub mod loki;
pub mod lumisel;
pub mod mars;
pub mod medusa;
pub mod mosscloak;
pub mod set_lord_of_chaos;
pub mod tyr;
pub mod zeus;

#[derive(Default)]
pub struct PluginChampions;

impl Plugin for PluginChampions {
    fn build(&self, app: &mut bevy::app::App) {
        app.add_plugins(erebos::PluginErebos);
        // ── Graduated Aeonrift champions ──
        app.add_plugins(zeus::PluginZeus);
        app.add_plugins(medusa::PluginMedusa);
        app.add_plugins(tyr::PluginTyr);
        app.add_plugins(loki::PluginLoki);
        app.add_plugins(mars::PluginMars);
        app.add_plugins(set_lord_of_chaos::PluginSet);
        app.add_plugins(kaeza::PluginKaeza);
        app.add_plugins(lumisel::PluginLumisel);
        app.add_plugins(dregmaw::PluginDregmaw);
        app.add_plugins(mosscloak::PluginMosscloak);
    }
}
