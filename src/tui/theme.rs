//! "Gentle Mist Blue" palette adapted from dsh-TUI's dark theme: mist blues
//! carry brand, focus, and interaction; body text stays warm off-white.
use ratatui::style::{Color, Modifier, Style};

/// Accent Soft — mist brand blue.
pub const ACCENT: Color = Color::Rgb(0x7D, 0xA1, 0xDE);
/// Border Blue — focus/selection, dialog accents.
pub const SUGGESTION: Color = Color::Rgb(0xAB, 0xC2, 0xEC);
/// Warm off-white body text.
pub const TEXT: Color = Color::Rgb(0xE8, 0xE6, 0xE0);
/// Mist gray-blue — secondary text.
pub const MUTED: Color = Color::Rgb(0x8D, 0x95, 0xA6);
/// Dimmer blue-gray — placeholders and hint lines.
pub const SUBTLE: Color = Color::Rgb(0x5E, 0x66, 0x73);
/// Mist green.
pub const SUCCESS: Color = Color::Rgb(0x82, 0xB8, 0x9D);
/// Soft rose.
pub const ERROR: Color = Color::Rgb(0xDA, 0x8A, 0x93);
/// Soft amber.
pub const WARNING: Color = Color::Rgb(0xD8, 0xB2, 0x70);
/// Muted sage green — plan mode.
pub const PLAN: Color = Color::Rgb(0x7F, 0xAE, 0x99);
/// Warm gold — user prompt label.
pub const USER: Color = Color::Rgb(0xFF, 0xDF, 0x80);
/// Muted blue-gray — input box border.
pub const PROMPT_BORDER: Color = Color::Rgb(0x55, 0x60, 0x6F);
/// Deep warm charcoal — text on filled badges.
pub const INVERSE: Color = Color::Rgb(0x22, 0x26, 0x2E);

pub fn text() -> Style {
    Style::default().fg(TEXT)
}
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}
pub fn subtle() -> Style {
    Style::default().fg(SUBTLE)
}
/// Keyboard hints stay visually subordinate; terminal cells share one font size.
pub fn key_hint() -> Style {
    subtle()
        .add_modifier(Modifier::DIM)
        .remove_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}
pub fn heading() -> Style {
    accent().add_modifier(Modifier::BOLD)
}
pub fn suggestion() -> Style {
    Style::default().fg(SUGGESTION)
}
pub fn success() -> Style {
    Style::default().fg(SUCCESS)
}
pub fn warning() -> Style {
    Style::default().fg(WARNING)
}
pub fn error() -> Style {
    Style::default().fg(ERROR)
}
pub fn plan() -> Style {
    Style::default().fg(PLAN)
}
pub fn user() -> Style {
    Style::default().fg(USER).add_modifier(Modifier::BOLD)
}
pub fn border() -> Style {
    Style::default().fg(PROMPT_BORDER)
}
/// Filled badge for the selected option in dialogs.
pub fn selected_badge() -> Style {
    Style::default()
        .fg(INVERSE)
        .bg(WARNING)
        .add_modifier(Modifier::BOLD)
}
