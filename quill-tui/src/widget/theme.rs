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
pub const GROUP: Style = Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD);
pub const BANNER: Style = Style::new().fg(Color::Black).bg(Color::Yellow);
pub const GUTTER: Style = Style::new().fg(Color::DarkGray);
pub const CELL_CURSOR: Style = Style::new().fg(Color::Black).bg(Color::Cyan);

/// Editor colours by tree-sitter capture name.
pub const SQL: &[(&str, &str)] = &[
    ("keyword", "#c792ea"),
    ("keyword.operator", "#c792ea"),
    ("conditional", "#c792ea"),
    ("storageclass", "#c792ea"),
    ("type.qualifier", "#c792ea"),
    ("string", "#c3e88d"),
    ("number", "#f78c6c"),
    ("float", "#f78c6c"),
    ("boolean", "#f78c6c"),
    ("comment", "#676e95"),
    ("function.call", "#82aaff"),
    ("type", "#ffcb6b"),
    ("type.builtin", "#ffcb6b"),
    ("field", "#eeffff"),
    ("variable", "#eeffff"),
    ("parameter", "#f07178"),
    ("attribute", "#ffcb6b"),
    ("operator", "#89ddff"),
    ("punctuation.bracket", "#89ddff"),
    ("punctuation.delimiter", "#89ddff"),
    ("word_highlight", "#3a3a3a"),
];
pub const ROW_CURSOR: Style = Style::new().bg(Color::Indexed(236));
