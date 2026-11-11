//! The terminal's colour for what the view writes the same in every frontend.

use ratatui::style::Color;

pub use dibstop_view::text::{cores, fit, plain};

pub fn agent_hue(name: &str) -> Color {
    Color::Indexed(dibstop_view::text::agent_hue(name))
}
