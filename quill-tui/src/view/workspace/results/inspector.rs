//! A popup with one cell's full value, wrapped, JSON pretty-printed.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    widgets::{Clear, Paragraph, Wrap},
};

use crate::widget::panel;

pub const HINTS: &[crate::component::KeyHint] = &[
    crate::component::KeyHint::new("y", "copy"),
    crate::component::KeyHint::new("j/k", "scroll"),
    crate::component::KeyHint::new("esc", "close"),
];
const SIZE_PERCENT: u16 = 80;

pub enum InspectorOutcome {
    Open,
    Close,
    Copy(String),
}

pub struct Inspector {
    title: String,
    text: String,
    scroll: u16,
    copied: bool,
}

impl Inspector {
    pub fn new(column: &str, row: u64, value: &str) -> Self {
        let trimmed = value.trim_start();
        let pretty = (trimmed.starts_with('{') || trimmed.starts_with('['))
            .then(|| serde_json::from_str::<serde_json::Value>(value).ok())
            .flatten()
            .and_then(|json| serde_json::to_string_pretty(&json).ok());
        Self {
            title: format!("{column} · row {row}"),
            text: pretty.unwrap_or_else(|| value.to_owned()),
            scroll: 0,
            copied: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InspectorOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return InspectorOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char('y') => {
                self.copied = true;
                return InspectorOutcome::Copy(self.text.clone());
            }
            _ => {}
        }
        InspectorOutcome::Open
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let [row] = Layout::vertical([Constraint::Percentage(SIZE_PERCENT)])
            .flex(Flex::Center)
            .areas(area);
        let [popup] = Layout::horizontal([Constraint::Percentage(SIZE_PERCENT)])
            .flex(Flex::Center)
            .areas(row);
        let title = if self.copied {
            format!("{} (copied)", self.title)
        } else {
            self.title.clone()
        };
        let paragraph = Paragraph::new(self.text.as_str())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .block(panel::block(&title, true));
        frame.render_widget(Clear, popup);
        frame.render_widget(paragraph, popup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_json_only() {
        assert_eq!(
            Inspector::new("c", 0, r#"{"a":1}"#).text,
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(Inspector::new("c", 0, "12").text, "12");
        assert_eq!(Inspector::new("c", 0, "{not json").text, "{not json");
    }
}
