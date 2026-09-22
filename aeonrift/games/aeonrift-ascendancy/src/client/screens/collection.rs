//! Collection surface for the mobile client.

use std::collections::BTreeMap;

use bevy::prelude::*;

use super::shop::ProductClientState;
use crate::client::actions::ShopOpen;

#[derive(Component)]
struct CollectionText;

pub struct CollectionScreenPlugin;

impl Plugin for CollectionScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_collection)
            .add_systems(Update, update_collection);
    }
}

fn spawn_collection(mut commands: Commands) {
    commands.spawn((
        Text2d::new("Collection"),
        TextFont {
            font_size: FontSize::Px(5.0),
            ..default()
        },
        TextColor(Color::WHITE),
        TextLayout::justify(Justify::Left),
        Transform::from_xyz(-30.0, 4.0, 31.0),
        Visibility::Hidden,
        CollectionText,
    ));
}

fn update_collection(
    shop: Res<ShopOpen>,
    state: Res<ProductClientState>,
    mut text: Query<(&mut Text2d, &mut Visibility), With<CollectionText>>,
) {
    let Ok((mut text, mut visibility)) = text.single_mut() else {
        return;
    };
    *visibility = if shop.0 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if shop.0 && (state.is_changed() || text.0 == "Collection") {
        text.0 = render_collection(&state.collection);
    }
}

fn render_collection(collection: &BTreeMap<String, u32>) -> String {
    let mut lines = vec!["COLLECTION".to_string()];
    for (slug, copies) in collection.iter().take(10) {
        lines.push(format!("{copies}×  {}", slug.replace('-', " ")));
    }
    if collection.len() > 10 {
        lines.push(format!("+ {} more", collection.len() - 10));
    }
    lines.join("\n")
}
