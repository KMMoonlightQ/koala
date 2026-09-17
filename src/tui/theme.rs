use ratatui::style::{Color, Modifier, Style};

pub fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}
pub fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}
pub fn heading() -> Style {
    accent().add_modifier(Modifier::BOLD)
}
pub fn success() -> Style {
    Style::default().fg(Color::Green)
}
pub fn warning() -> Style {
    Style::default().fg(Color::Yellow)
}
pub fn error() -> Style {
    Style::default().fg(Color::Red)
}
