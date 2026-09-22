//! Action-based input for the card client, via `leafwing-input-manager` (Bevy 0.19).
//!
//! The card game's input verbs — pick one of the four hand cards, confirm a deploy,
//! or open the shop — are expressed as a [`CardAction`] `Actionlike` and read as an
//! `ActionState`, instead of scattering raw `KeyCode` checks through the client. This
//! is the VIEW layer: it only ever calls the deterministic core's `crate::try_deploy`;
//! the authoritative rules (elixir cost, lane legality) stay in the core.
//!
//! It AUGMENTS, and does not replace, the pointer/touch flow in [`super::input`]:
//! tapping remains the primary mobile interaction (select a card, tap a lane), while
//! these actions add keyboard control on desktop (digits select, Space deploys into
//! the centre lane, S toggles the shop). On mobile the same verbs back on-screen HUD
//! buttons.
//!
//! ## The elixir "pool" (client presentation mirror)
//! [`ClientElixir`] is the client-side [`crate::rules::elixir`] mirror — the
//! `leafwing_abilities::Pool` analogue for this game's resource. It is refreshed each
//! frame from the authoritative `MatchState` and used only for immediate UX feedback
//! (e.g. refusing a deploy press with an empty pool); the real spend happens in
//! `try_deploy`. (`leafwing_abilities` itself only publishes Bevy 0.18-rc/older
//! builds, so it can't join this Bevy-0.19 client — hence this tiny local mirror.)

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

use crate::{try_deploy, Deploy};

use super::game::{Battle, Selection, StatusMessage, HUMAN};
use super::input::error_message;

/// The card client's high-level input verbs.
#[derive(Actionlike, PartialEq, Eq, Hash, Clone, Copy, Debug, Reflect)]
pub enum CardAction {
    /// Select the 1st..4th card in hand.
    SelectCard1,
    SelectCard2,
    SelectCard3,
    SelectCard4,
    /// Deploy the selected card into the centre lane (keyboard confirm).
    DeployToLane,
    /// Toggle the item/card shop overlay.
    OpenShop,
    /// Buy the first visible pack offer (desktop/test binding; touch UI emits the same verb).
    BuyPack,
}

impl CardAction {
    /// The hand-card index a `SelectCardN` action selects (0-based), if any.
    pub fn card_index(self) -> Option<usize> {
        match self {
            CardAction::SelectCard1 => Some(0),
            CardAction::SelectCard2 => Some(1),
            CardAction::SelectCard3 => Some(2),
            CardAction::SelectCard4 => Some(3),
            _ => None,
        }
    }
}

/// The default desktop keyboard binding: digits 1-4 select, Space deploys, S shops.
pub fn card_input_map() -> InputMap<CardAction> {
    InputMap::default()
        .with(CardAction::SelectCard1, KeyCode::Digit1)
        .with(CardAction::SelectCard2, KeyCode::Digit2)
        .with(CardAction::SelectCard3, KeyCode::Digit3)
        .with(CardAction::SelectCard4, KeyCode::Digit4)
        .with(CardAction::DeployToLane, KeyCode::Space)
        .with(CardAction::OpenShop, KeyCode::KeyS)
        .with(CardAction::BuyPack, KeyCode::KeyB)
}

/// The client-side elixir pool mirror (presentation only). Refreshed each frame from
/// the authoritative `MatchState`; the real economy lives in `crate::rules::elixir`.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ClientElixir {
    pub current: u32,
    pub max: u32,
}

impl ClientElixir {
    /// Whether the pool currently shows enough elixir to pay `cost`.
    pub fn can_afford(&self, cost: u32) -> bool {
        self.current >= cost
    }
}

/// Whether the shop overlay is currently open (toggled by [`CardAction::OpenShop`]).
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ShopOpen(pub bool);

/// Wires the card-action VIEW into the client `App`: the input plugin, the local
/// controller (holding the [`InputMap`]), the elixir mirror + shop-toggle state, and
/// the read systems.
#[derive(Default)]
pub struct CardInputPlugin;

impl Plugin for CardInputPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(InputManagerPlugin::<CardAction>::default())
            .init_resource::<ClientElixir>()
            .init_resource::<ShopOpen>()
            .add_systems(Startup, spawn_card_controller)
            .add_systems(Update, (sync_client_elixir, handle_action_input).chain());
    }
}

/// Spawn the entity carrying the local player's [`InputMap`] (+ its `ActionState`).
fn spawn_card_controller(mut commands: Commands) {
    commands.spawn((
        card_input_map(),
        ActionState::<CardAction>::default(),
        Name::new("CardLocalController"),
    ));
}

/// Mirror the authoritative human elixir into the client [`ClientElixir`] pool.
fn sync_client_elixir(battle: Res<Battle>, mut pool: ResMut<ClientElixir>) {
    pool.current = battle.state.elixir[HUMAN as usize];
    pool.max = battle.state.config.max_elixir;
}

/// Read the card `ActionState` and drive selection / deploy / shop:
/// - `SelectCardN` selects (or toggles off) that hand card,
/// - `DeployToLane` deploys the selected card into the centre lane via `try_deploy`,
/// - `OpenShop` toggles the shop overlay.
fn handle_action_input(
    controller: Query<&ActionState<CardAction>>,
    mut battle: ResMut<Battle>,
    mut selection: ResMut<Selection>,
    mut status: ResMut<StatusMessage>,
    mut shop: ResMut<ShopOpen>,
    mut shop_commands: ResMut<super::screens::shop::ShopCommandQueue>,
    product: Res<super::screens::shop::ProductClientState>,
    pool: Res<ClientElixir>,
) {
    let Ok(state) = controller.single() else {
        return;
    };
    let deck_len = battle.state.decks[HUMAN as usize].len();

    // 1) Card selection (digits 1-4).
    for action in [
        CardAction::SelectCard1,
        CardAction::SelectCard2,
        CardAction::SelectCard3,
        CardAction::SelectCard4,
    ] {
        if state.just_pressed(&action) {
            if let Some(idx) = action.card_index() {
                if idx < deck_len {
                    selection.card = if selection.card == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                }
            }
        }
    }

    // 2) Shop toggle.
    if state.just_pressed(&CardAction::OpenShop) {
        shop.0 = !shop.0;
        status.set(if shop.0 { "Shop opened" } else { "Shop closed" });
    }

    if state.just_pressed(&CardAction::BuyPack) && shop.0 {
        if let Some(offer) = product.offers.first() {
            shop_commands.0.push(super::screens::shop::PurchasePackCommand {
                pack_id: offer.pack_id.clone(),
                idempotency_key: format!(
                    "client-{}-{}",
                    offer.pack_id,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|value| value.as_millis())
                        .unwrap_or(0)
                ),
            });
            status.set("Purchase queued — server will pay and roll the pack");
        }
    }

    // 3) Deploy the selected card into the centre lane.
    if state.just_pressed(&CardAction::DeployToLane) {
        let Some(card) = selection.card else {
            status.set("Select a card first");
            return;
        };
        let lane = (battle.state.config.lanes.max(1) / 2).min(battle.state.config.lanes.max(1) - 1);
        // Client pool pre-check for instant UX feedback; `try_deploy` is authoritative.
        let cost = battle.state.decks[HUMAN as usize][card].meta.cost;
        if !pool.can_afford(cost) {
            status.set(format!("Not enough elixir ({}/{})", pool.current, cost));
            return;
        }
        let deploy = Deploy::new(HUMAN, card, lane);
        match try_deploy(&mut battle.state, deploy) {
            Ok(id) => {
                let name = battle.state.decks[HUMAN as usize][card].name().to_string();
                if let Some(slug) = super::game::slug_for_card(card) {
                    battle.record_unit(id, slug);
                }
                status.set(format!("Deployed {name} -> lane {}", lane + 1));
                selection.card = None;
            }
            Err(e) => status.set(error_message(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_actions_map_to_card_indices() {
        assert_eq!(CardAction::SelectCard1.card_index(), Some(0));
        assert_eq!(CardAction::SelectCard4.card_index(), Some(3));
        assert_eq!(CardAction::DeployToLane.card_index(), None);
        assert_eq!(CardAction::OpenShop.card_index(), None);
    }

    #[test]
    fn input_map_binds_every_action() {
        let map = card_input_map();
        for action in [
            CardAction::SelectCard1,
            CardAction::SelectCard2,
            CardAction::SelectCard3,
            CardAction::SelectCard4,
            CardAction::DeployToLane,
            CardAction::OpenShop,
        ] {
            assert!(
                map.get_buttonlike(&action).map_or(false, |b| !b.is_empty()),
                "action {action:?} should have a binding"
            );
        }
    }

    #[test]
    fn client_elixir_affordability() {
        let pool = ClientElixir { current: 5, max: 10 };
        assert!(pool.can_afford(5));
        assert!(!pool.can_afford(6));
    }
}
