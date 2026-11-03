//! Reads the synced biome registry: how many biomes there are and each one's
//! climate. A connection reads it off the wire and the viewer is handed the
//! same shape by its world source.

use azalea_core::registry_holder::{RegistryHolder, RegistryType};
use azalea_registry::identifier::Identifier;
use simdnbt::owned::{NbtCompound, NbtTag};

use crate::renderer::chunk::mesher::{BiomeClimate, GrassColorModifier, int_to_rgb};

/// The synced biome registry, in protocol-id order.
pub(crate) fn biome_registry(holder: &RegistryHolder) -> Option<&RegistryType<NbtCompound>> {
    holder.extra.get(&Identifier::new("minecraft:worldgen/biome"))
}

/// How many biomes chunk sections can name.
pub(crate) fn biome_count(holder: &RegistryHolder) -> usize {
    biome_registry(holder).map_or(0, |registry| registry.map.len())
}

pub(crate) fn extract_biome_climate(
    holder: &RegistryHolder,
) -> std::collections::HashMap<u32, BiomeClimate> {
    let mut result = std::collections::HashMap::new();
    if let Some(registry) = biome_registry(holder) {
        for (id, (_, nbt)) in registry.map.iter().enumerate() {
            let temp = nbt_float(nbt, "temperature").unwrap_or(0.8);
            let downfall = nbt_float(nbt, "downfall").unwrap_or(0.4);

            let effects = nbt.get("effects").and_then(|v| match v {
                NbtTag::Compound(c) => Some(c),
                _ => None,
            });

            let grass_color_override = effects
                .and_then(|e| nbt_color_from_compound(e, "grass_color"))
                .map(int_to_rgb);

            let foliage_color_override = effects
                .and_then(|e| nbt_color_from_compound(e, "foliage_color"))
                .map(int_to_rgb);

            let dry_foliage_color_override = effects
                .and_then(|e| nbt_color_from_compound(e, "dry_foliage_color"))
                .map(int_to_rgb);

            let water_color = effects
                .and_then(|e| nbt_color_from_compound(e, "water_color"))
                .map(int_to_rgb)
                .unwrap_or_else(|| int_to_rgb(0x3F76E4));

            let grass_color_modifier = effects
                .and_then(|e| nbt_string_from_compound(e, "grass_color_modifier"))
                .map(|s| match s.as_str() {
                    "dark_forest" => GrassColorModifier::DarkForest,
                    "swamp" => GrassColorModifier::Swamp,
                    _ => GrassColorModifier::None,
                })
                .unwrap_or(GrassColorModifier::None);

            result.insert(
                id as u32,
                BiomeClimate {
                    temperature: temp,
                    downfall,
                    grass_color_override,
                    grass_color_modifier,
                    foliage_color_override,
                    dry_foliage_color_override,
                    water_color,
                },
            );
        }
    }
    tracing::info!("Extracted {} biome climate entries", result.len());
    result
}

fn nbt_float(nbt: &NbtCompound, key: &str) -> Option<f32> {
    nbt.get(key).and_then(|v| match v {
        NbtTag::Float(f) => Some(*f),
        NbtTag::Double(d) => Some(*d as f32),
        _ => None,
    })
}

fn nbt_color_from_compound(compound: &NbtCompound, key: &str) -> Option<i32> {
    compound.get(key).and_then(|v| match v {
        NbtTag::Int(i) => Some(*i),
        NbtTag::Long(l) => Some(*l as i32),
        NbtTag::String(s) => {
            let s = s.to_string();
            let hex = s.strip_prefix('#').unwrap_or(&s);
            i32::from_str_radix(hex, 16).ok()
        }
        _ => None,
    })
}

fn nbt_string_from_compound(compound: &NbtCompound, key: &str) -> Option<String> {
    compound.get(key).and_then(|v| match v {
        NbtTag::String(s) => Some(s.to_string()),
        _ => None,
    })
}
