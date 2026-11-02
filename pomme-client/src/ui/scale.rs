//! GUI scaling shared by the renderer and the menus.

/// Vanilla `Window.calculateScale`: the largest scale that keeps the screen at
/// least 320x240, capped by `setting` (0 is Auto), then made even while Force
/// Unicode Font is on.
pub fn gui_scale(screen_w: f32, screen_h: f32, setting: u32, enforce_unicode: bool) -> f32 {
    let mut scale = 1;
    while scale != setting
        && screen_w / (scale + 1) as f32 >= 320.0
        && screen_h / (scale + 1) as f32 >= 240.0
    {
        scale += 1;
    }
    if enforce_unicode && scale % 2 != 0 {
        scale += 1;
    }
    scale as f32
}

/// Vanilla `Options.menuBackgroundBlurriness`'s default.
pub const DEFAULT_MENU_BACKGROUND_BLURRINESS: u8 = 5;
