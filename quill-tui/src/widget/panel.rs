//! Bordered panels whose border shows focus.
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, BorderType, Paragraph},
};

use super::theme;

pub fn block(title: &str, focused: bool) -> Block<'static> {
    let (border, style) = if focused {
        (BorderType::Thick, theme::FOCUSED_BORDER)
    } else {
        (BorderType::Plain, theme::BORDER)
    };
    Block::bordered()
        .border_type(border)
        .border_style(style)
        .title(format!(" {title} "))
        .title_style(theme::TITLE)
}

/// A panel whose content is not built yet.
pub fn placeholder(frame: &mut Frame, area: Rect, title: &str, focused: bool, note: &'static str) {
    let paragraph = Paragraph::new(note)
        .style(theme::DIM)
        .block(block(title, focused));
    frame.render_widget(paragraph, area);
}
