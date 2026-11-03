//! The UI the viewer draws when it is switched on: the F3 screen, from the
//! `debugger` crate's lines.

use std::collections::HashMap;

use ui::{UiError, UiFrameRenderer};

use crate::renderer::pipelines::menu_overlay::MenuElement;
use crate::ui::common::push_debug_lines;

/// What the overlay is drawn from and into: the frame's GUI scale, the width
/// of each line as the renderer measured it, and the elements it produces.
pub struct OverlayFrame {
    pub scale: f32,
    pub font_size: f32,
    pub lines: Vec<String>,
    pub widths: HashMap<String, f32>,
    pub elements: Vec<MenuElement>,
}

pub struct DebugOverlay;

impl UiFrameRenderer<OverlayFrame> for DebugOverlay {
    fn render_ui(&mut self, frame: &mut OverlayFrame) -> Result<(), UiError> {
        let pad = 4.0 * frame.scale;
        let widths = &frame.widths;
        push_debug_lines(
            &mut frame.elements,
            frame.lines.clone(),
            pad,
            pad,
            frame.font_size,
            true,
            &|line, _| widths.get(line).copied().unwrap_or(0.0),
        );
        Ok(())
    }
}
