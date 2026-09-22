//! Canonical mist-blue styles resolved to terminal, light or dark colors on
//! the completed frame. Cached content stays independent of the chosen theme.
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
/// Consistent focus treatment for navigable lists, distinct from warning badges.
pub fn selected() -> Style {
    suggestion().add_modifier(Modifier::BOLD)
}

pub fn selection() -> Style {
    Style::default().fg(INVERSE).bg(SUGGESTION)
}

pub fn task_status(state: crate::agent::event::TaskState) -> Style {
    use crate::agent::event::TaskState;
    match state {
        TaskState::Running => accent(),
        TaskState::Stopping | TaskState::Stopped => warning(),
        TaskState::Done => success(),
        TaskState::Failed => error(),
    }
}

/// Filled badge for the selected option in dialogs.
pub fn selected_badge() -> Style {
    Style::default()
        .fg(INVERSE)
        .bg(WARNING)
        .add_modifier(Modifier::BOLD)
}

/// Renderers and transcript caches use the canonical dark palette. Resolve it
/// once on the completed frame so cached Markdown, overlays and editor styles
/// switch together without global state or rebuilding the conversation.
pub(super) fn apply(buffer: &mut ratatui::buffer::Buffer, theme: crate::config::Theme) {
    for cell in &mut buffer.content {
        cell.fg = foreground(theme, cell.fg);
        cell.bg = if cell.bg == Color::Reset {
            background(theme)
        } else {
            foreground(theme, cell.bg)
        };
    }
}

pub(super) fn background(theme: crate::config::Theme) -> Color {
    use crate::config::Theme;
    if let Some(palette) = palette(theme) {
        return palette[0];
    }
    match theme {
        Theme::Auto => Color::Reset,
        Theme::Light => Color::Rgb(0xFA, 0xFA, 0xF7),
        Theme::Dark => INVERSE,
        _ => unreachable!(),
    }
}

pub(super) fn foreground(theme: crate::config::Theme, color: Color) -> Color {
    use crate::config::Theme;
    if let Some(p) = palette(theme) {
        return match color {
            Color::Reset | TEXT => p[1],
            ACCENT => p[2],
            SUGGESTION => p[3],
            MUTED => p[4],
            SUBTLE | PROMPT_BORDER => p[5],
            SUCCESS | PLAN => p[6],
            ERROR => p[7],
            WARNING | USER => p[8],
            // These light palettes have bright amber badges; use charcoal ink.
            INVERSE if matches!(theme, Theme::CatppuccinLatte | Theme::SolarizedLight) => INVERSE,
            INVERSE => p[0],
            _ => color,
        };
    }
    match theme {
        Theme::Dark => match color {
            Color::Reset => TEXT,
            _ => color,
        },
        Theme::Light => match color {
            Color::Reset | TEXT => Color::Rgb(0x25, 0x2B, 0x35),
            ACCENT => Color::Rgb(0x2E, 0x59, 0x91),
            SUGGESTION => Color::Rgb(0x34, 0x55, 0x88),
            MUTED => Color::Rgb(0x55, 0x60, 0x70),
            SUBTLE => Color::Rgb(0x60, 0x68, 0x75),
            SUCCESS | PLAN => Color::Rgb(0x29, 0x69, 0x4D),
            ERROR => Color::Rgb(0xA3, 0x30, 0x44),
            WARNING | USER => Color::Rgb(0x7A, 0x53, 0x10),
            PROMPT_BORDER => Color::Rgb(0x76, 0x80, 0x90),
            INVERSE => Color::Rgb(0xFF, 0xFF, 0xFF),
            _ => color,
        },
        // Terminal-owned ANSI colors remain live references: changing the
        // terminal palette updates the UI even without another application draw.
        Theme::Auto => match color {
            TEXT => Color::Reset,
            ACCENT | USER => Color::Blue,
            SUGGESTION => Color::Cyan,
            MUTED | SUBTLE | PROMPT_BORDER => Color::DarkGray,
            SUCCESS | PLAN => Color::Green,
            ERROR => Color::Red,
            WARNING => Color::Yellow,
            INVERSE => Color::Black,
            _ => color,
        },
        _ => unreachable!(),
    }
}

// Official palettes: catppuccin.com/palette, nordtheme.com/docs/colors-and-palettes,
// draculatheme.com/spec. Roles: background, text, accent, focus, muted, border,
// success, error, warning. `catppuccin` keeps the original Mocha flavor.
// Light palettes: catppuccin.com/palette, ethanschoonover.com/solarized,
// github.com/primer/primitives (base/color/light).
fn palette(theme: crate::config::Theme) -> Option<[Color; 9]> {
    use crate::config::Theme;
    let colors = match theme {
        Theme::Catppuccin => [
            0x1e1e2e, 0xcdd6f4, 0x89b4fa, 0xcba6f7, 0xa6adc8, 0x6c7086, 0xa6e3a1, 0xf38ba8,
            0xf9e2af,
        ],
        Theme::Nord => [
            0x2e3440, 0xeceff4, 0x88c0d0, 0x81a1c1, 0xd8dee9, 0x4c566a, 0xa3be8c, 0xbf616a,
            0xebcb8b,
        ],
        Theme::Dracula => [
            0x282a36, 0xf8f8f2, 0xbd93f9, 0x8be9fd, 0x6272a4, 0x6272a4, 0x50fa7b, 0xff5555,
            0xf1fa8c,
        ],
        Theme::CatppuccinLatte => [
            0xeff1f5, 0x4c4f69, 0x1e66f5, 0x8839ef, 0x5c5f77, 0x6c6f85, 0x40a02b, 0xd20f39,
            0xdf8e1d,
        ],
        Theme::SolarizedLight => [
            0xfdf6e3, 0x657b83, 0x268bd2, 0x6c71c4, 0x657b83, 0x839496, 0x859900, 0xdc322f,
            0xb58900,
        ],
        Theme::GithubLight => [
            0xffffff, 0x1f2328, 0x0969da, 0x8250df, 0x59636e, 0x59636e, 0x1a7f37, 0xcf222e,
            0x9a6700,
        ],
        _ => return None,
    };
    Some(colors.map(|c| Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)))
}
