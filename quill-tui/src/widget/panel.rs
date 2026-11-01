//! Bordered panels whose border shows focus.
use ratatui::widgets::{Block, BorderType};

use super::theme;

pub fn block(title: &str, focused: bool) -> Block<'static> {
    frame(focused)
        .title(format!(" {title} "))
        .title_style(theme::TITLE)
}

/// A panel border without a title, for callers that build their own.
pub fn frame(focused: bool) -> Block<'static> {
    let (border, style) = if focused {
        (BorderType::Thick, theme::FOCUSED_BORDER)
    } else {
        (BorderType::Plain, theme::BORDER)
    };
    Block::bordered().border_type(border).border_style(style)
}
