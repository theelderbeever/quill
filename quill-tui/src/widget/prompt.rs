//! Small centred dialogs drawn over a view.
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use super::{input::TextInput, panel, theme};
use crate::component::KeyHint;

const WIDTH: u16 = 50;

fn centered(area: Rect, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(WIDTH.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// A titled box asking for text; places the terminal cursor in it.
pub fn render_input(frame: &mut Frame, area: Rect, title: &str, input: &TextInput) {
    let area = centered(area, 3);
    let block = panel::block(title, true);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(input.text()).block(block), area);
    let x = inner
        .x
        .saturating_add(input.width())
        .min(inner.right().saturating_sub(1));
    frame.set_cursor_position(Position::new(x, inner.y));
}

/// A titled box listing `keys`, one per line.
pub fn render_keys(frame: &mut Frame, area: Rect, title: &str, keys: &[KeyHint]) {
    let key_width = keys.iter().map(|k| k.key.len()).max().unwrap_or(0);
    let lines: Vec<Line> = keys
        .iter()
        .map(|k| {
            Line::from(vec![
                Span::styled(format!("<{}>", k.key), theme::KEY),
                Span::raw(" ".repeat(key_width - k.key.len() + 1)),
                Span::styled(k.action, theme::DIM),
            ])
        })
        .collect();
    let area = centered(area, u16::try_from(lines.len()).unwrap_or(u16::MAX) + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(panel::block(title, true)), area);
}

/// A titled box asking yes or no.
pub fn render_confirm(frame: &mut Frame, area: Rect, title: &str, question: &str) {
    let area = centered(area, 4);
    let lines = vec![
        Line::raw(question),
        Line::styled("y confirm · n cancel", theme::DIM),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(panel::block(title, true)), area);
}
