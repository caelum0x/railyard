//! Shop / pack-opening surface. It renders server-provided offers and collection
//! state and emits idempotent purchase commands; the server owns Mint payment,
//! RNG, and entitlement grants.

use std::collections::BTreeMap;
use bevy::prelude::*;

use crate::client::actions::ShopOpen;

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
            .add_systems(Update, update_shop);
    }
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
