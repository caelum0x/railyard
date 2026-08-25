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

// ── Auto-ported Aeonrift champions (translation-map port) ──
pub mod achilles;
pub mod anubis_warden_of_scales;
pub mod artemis;
pub mod athena;
pub mod bastet_claws_of_the_hunt;
pub mod bellona;
pub mod brackmoor;
pub mod cyranth;
pub mod freyja;
pub mod gorehoof;
pub mod hades;
pub mod horus_skyfall_talon;
pub mod isis_mother_of_magic;
pub mod janus;
pub mod korrathil;
pub mod mercury;
pub mod neptune;
pub mod nerreth;
pub mod nyxralla;
pub mod odin;
pub mod petalsong;
pub mod pyromancer;
pub mod quorvane;
pub mod ra_radiant_dawn;
pub mod sentinel;
pub mod sporeveil;
pub mod thor;
pub mod thornquill;
pub mod toran;
pub mod ullr;
pub mod vanguard;
pub mod vashka;
pub mod veyl;
pub mod vulcan;
pub mod xelphine;
pub mod ysmera;
pub mod zeryth;

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
        // ── Auto-ported champions ──
        app.add_plugins(achilles::PluginAchilles);
        app.add_plugins(anubis_warden_of_scales::PluginAnubis);
        app.add_plugins(artemis::PluginArtemis);
        app.add_plugins(athena::PluginAthena);
        app.add_plugins(bastet_claws_of_the_hunt::PluginBastet);
        app.add_plugins(bellona::PluginBellona);
        app.add_plugins(brackmoor::PluginBrackmoor);
        app.add_plugins(cyranth::PluginCyranth);
        app.add_plugins(freyja::PluginFreyja);
        app.add_plugins(gorehoof::PluginGorehoof);
        app.add_plugins(hades::PluginHades);
        app.add_plugins(horus_skyfall_talon::PluginHorus);
        app.add_plugins(isis_mother_of_magic::PluginIsis);
        app.add_plugins(janus::PluginJanus);
        app.add_plugins(korrathil::PluginKorrathil);
        app.add_plugins(mercury::PluginMercury);
        app.add_plugins(neptune::PluginNeptune);
        app.add_plugins(nerreth::PluginNerreth);
        app.add_plugins(nyxralla::PluginNyxralla);
        app.add_plugins(odin::PluginOdin);
        app.add_plugins(petalsong::PluginPetalsong);
        app.add_plugins(pyromancer::PluginPyromancer);
        app.add_plugins(quorvane::PluginQuorvane);
        app.add_plugins(ra_radiant_dawn::PluginRa);
        app.add_plugins(sentinel::PluginSentinel);
        app.add_plugins(sporeveil::PluginSporeveil);
        app.add_plugins(thor::PluginThor);
        app.add_plugins(thornquill::PluginThornquill);
        app.add_plugins(toran::PluginToran);
        app.add_plugins(ullr::PluginUllr);
        app.add_plugins(vanguard::PluginVanguard);
        app.add_plugins(vashka::PluginVashka);
        app.add_plugins(veyl::PluginVeyl);
        app.add_plugins(vulcan::PluginVulcan);
        app.add_plugins(xelphine::PluginXelphine);
        app.add_plugins(ysmera::PluginYsmera);
        app.add_plugins(zeryth::PluginZeryth);
    }
}
