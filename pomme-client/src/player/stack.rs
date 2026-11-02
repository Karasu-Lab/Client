//! Item stack component lookups shared by the renderer and the interaction
//! code.

use azalea_inventory::ItemStackData;
use azalea_inventory::components::ChargedProjectiles;
use azalea_inventory::default_components::{DefaultableComponent, get_default_component};
use azalea_registry::builtin::ItemKind;

/// Vanilla `ItemInHandRenderer.isChargedCrossbow` / `CrossbowItem.isCharged`.
pub(crate) fn is_charged_crossbow(stack: &ItemStackData) -> bool {
    stack.kind == ItemKind::Crossbow
        && stack_component::<ChargedProjectiles>(stack).is_some_and(|c| !c.items.is_empty())
}

/// The stack's component override if the server set one, else the item's
/// default.
pub(crate) fn stack_component<T: DefaultableComponent + Clone>(stack: &ItemStackData) -> Option<T> {
    stack
        .component_patch
        .get::<T>()
        .cloned()
        .or_else(|| get_default_component::<T>(stack.kind))
}
