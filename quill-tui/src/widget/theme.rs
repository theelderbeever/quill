//! Named styles, so views agree on what focus, errors and hints look like.
use ratatui::style::{Color, Modifier, Style};

pub const BORDER: Style = Style::new().fg(Color::DarkGray);
pub const FOCUSED_BORDER: Style = Style::new().fg(Color::Cyan);
pub const TITLE: Style = Style::new().add_modifier(Modifier::BOLD);
pub const DIM: Style = Style::new().fg(Color::DarkGray);
pub const KEY: Style = Style::new().fg(Color::Cyan);
pub const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);
pub const LOGO: Style = Style::new().fg(Color::Yellow);
pub const OK: Style = Style::new().fg(Color::Green);
pub const PENDING: Style = Style::new().fg(Color::Yellow);
pub const ERROR: Style = Style::new().fg(Color::Red);
