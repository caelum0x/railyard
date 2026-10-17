//! Shop / pack-opening surface. It renders server-provided offers and collection
//! state and emits idempotent purchase commands; the server owns Mint payment,
//! RNG, and entitlement grants.
//!
//! The client is network-less today (no `server::product::ProductStore` wired over
//! HTTP from this crate yet — see `games/aeonrift-ascendancy/src/server/product.rs`),
//! so [`apply_shop_commands`] resolves a queued [`PurchasePackCommand`] locally: it
//! opens the same deterministic [`crate::cards::pack`] a live server would, using the
//! shared card pool, and grants the drops straight into [`ProductClientState`]. This
//! keeps "press B to buy" from being a dead button that queues a command nothing ever
//! drains — once the real Mint-backed HTTP purchase lands, this local resolution is the
//! seam to replace.

use std::collections::BTreeMap;
use bevy::prelude::*;

use crate::client::actions::ShopOpen;
use crate::client::game::StatusMessage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferView {
    pub pack_id: String,
    pub title: String,
    pub price_micros: i64,
}

#[derive(Resource, Clone, Debug)]
pub struct ProductClientState {
    pub collection: BTreeMap<String, u32>,
    pub offers: Vec<OfferView>,
    pub last_opened: Vec<String>,
}

impl Default for ProductClientState {
    fn default() -> Self {
        let collection = crate::client::game::DECK_SLUGS
            .iter()
            .map(|slug| ((*slug).to_string(), 1))
            .collect();
        Self {
            collection,
            offers: vec![OfferView {
                pack_id: "standard".to_string(),
                title: "Standard Pack".to_string(),
                price_micros: 990_000,
            }],
            last_opened: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurchasePackCommand {
    pub pack_id: String,
    pub idempotency_key: String,
}

#[derive(Resource, Default)]
pub struct ShopCommandQueue(pub Vec<PurchasePackCommand>);

#[derive(Component)]
struct ShopBackdrop;

#[derive(Component)]
struct ShopText;

/// Plugin for the shop screen.
pub struct ShopScreenPlugin;

impl Plugin for ShopScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProductClientState>()
            .init_resource::<ShopCommandQueue>()
            .add_systems(Startup, spawn_shop)
            .add_systems(Update, (apply_shop_commands, update_shop).chain());
    }
}

/// Drain queued purchase commands and resolve each one locally: open the requested
/// pack with the shared deterministic pack-opening logic and grant the drops into the
/// client's collection. Without this, [`PurchasePackCommand`]s pushed by
/// `actions::handle_action_input` accumulate in [`ShopCommandQueue`] forever and the
/// "Purchase queued" status never resolves — the shop looks interactive but does
/// nothing.
fn apply_shop_commands(
    mut queue: ResMut<ShopCommandQueue>,
    mut state: ResMut<ProductClientState>,
    mut status: ResMut<StatusMessage>,
) {
    if queue.0.is_empty() {
        return;
    }
    let commands = std::mem::take(&mut queue.0);
    for command in commands {
        match open_local_pack(&command.pack_id) {
            Ok(slugs) if !slugs.is_empty() => {
                for slug in &slugs {
                    *state.collection.entry(slug.clone()).or_insert(0) += 1;
                }
                status.set(format!("Opened {}: {}", command.pack_id, slugs.join(", ")));
                state.last_opened = slugs;
            }
            Ok(_) => status.set(format!("Pack '{}' had no cards to grant", command.pack_id)),
            Err(e) => status.set(format!("Could not open pack '{}': {e}", command.pack_id)),
        }
    }
}

/// Open `pack_id` with a fresh random seed against the shared card pool, returning the
/// granted cards' slugs (the same name->slug mapping the authoritative server-side
/// `purchase_pack` uses).
fn open_local_pack(pack_id: &str) -> Result<Vec<String>, crate::cards::CardError> {
    let pool = crate::cards::CardPool::load_default()?;
    let pack = crate::cards::load_pack(crate::CONTENT_CARDS_DIR, pack_id)?;
    let seed: u64 = rand::random();
    let drops = crate::cards::open_pack(&pool, &pack, seed);
    let name_to_slug: BTreeMap<_, _> = pool
        .entries()
        .iter()
        .map(|entry| (entry.card.name().to_string(), entry.slug.clone()))
        .collect();
    Ok(drops
        .iter()
        .filter_map(|card| name_to_slug.get(card.name()).cloned())
        .collect())
}

fn spawn_shop(mut commands: Commands) {
    commands.spawn((
        Sprite::from_color(Color::srgba(0.025, 0.02, 0.06, 0.96), Vec2::new(78.0, 118.0)),
        Transform::from_xyz(0.0, 0.0, 30.0),
        Visibility::Hidden,
        ShopBackdrop,
    ));
    commands.spawn((
        Text2d::new("Shop"),
        TextFont {
            font_size: FontSize::Px(6.0),
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.8, 0.35)),
        TextLayout::justify(Justify::Left),
        Transform::from_xyz(8.0, 42.0, 31.0),
        Visibility::Hidden,
        ShopText,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_local_pack_grants_real_slugs_from_the_pool() {
        let slugs = open_local_pack("standard").expect("standard pack opens locally");
        assert!(!slugs.is_empty(), "a pack must grant at least one card");
        let pool = crate::cards::CardPool::load_default().expect("pool loads");
        for slug in &slugs {
            assert!(
                pool.entries().iter().any(|entry| &entry.slug == slug),
                "granted slug {slug} must be a real pool card"
            );
        }
    }

    /// Regression test for the dead-button bug: queuing a purchase must actually
    /// grow the collection and clear the queue, not sit there forever.
    #[test]
    fn apply_shop_commands_drains_the_queue_and_grants_cards() {
        let mut app = App::new();
        app.init_resource::<ShopCommandQueue>()
            .init_resource::<ProductClientState>()
            .init_resource::<StatusMessage>()
            .add_systems(Update, apply_shop_commands);

        let before: u32 = app
            .world()
            .resource::<ProductClientState>()
            .collection
            .values()
            .sum();

        app.world_mut()
            .resource_mut::<ShopCommandQueue>()
            .0
            .push(PurchasePackCommand {
                pack_id: "standard".to_string(),
                idempotency_key: "test-key-1".to_string(),
            });

        app.update();

        let state = app.world().resource::<ProductClientState>();
        let after: u32 = state.collection.values().sum();
        assert!(
            after > before,
            "buying a pack must grow the collection (before={before}, after={after})"
        );
        assert!(
            !state.last_opened.is_empty(),
            "a successful purchase must record what was opened"
        );
        assert!(
            app.world().resource::<ShopCommandQueue>().0.is_empty(),
            "the command queue must be drained after processing"
        );
    }
}

fn update_shop(
    shop: Res<ShopOpen>,
    state: Res<ProductClientState>,
    mut backdrop: Query<&mut Visibility, (With<ShopBackdrop>, Without<ShopText>)>,
    mut label: Query<(&mut Text2d, &mut Visibility), With<ShopText>>,
) {
    let visible = if shop.0 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if let Ok(mut visibility) = backdrop.single_mut() {
        *visibility = visible;
    }
    let Ok((mut text, mut visibility)) = label.single_mut() else {
        return;
    };
    *visibility = visible;
    if shop.0 && (state.is_changed() || text.0 == "Shop") {
        let mut lines = vec!["RIFT MARKET".to_string()];
        for offer in &state.offers {
            lines.push(format!(
                "{}\n{} AIT  •  press B to buy",
                offer.title,
                offer.price_micros as f64 / 1_000_000.0
            ));
        }
        if !state.last_opened.is_empty() {
            lines.push(format!("OPENED\n{}", state.last_opened.join(", ")));
        }
        text.0 = lines.join("\n\n");
    }
}
