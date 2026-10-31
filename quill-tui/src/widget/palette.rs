//! The `:` command line, which also shows the latest message.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{Block, Widget},
};

use super::{panel, theme};

/// Bordered, one line of text.
pub const HEIGHT: u16 = 3;

pub enum Flash {
    Error(String),
}

pub struct Palette {
    /// Present while the palette is open.
    input: Option<String>,
    flash: Option<Flash>,
}

impl Palette {
    pub const fn new() -> Self {
        Self {
            input: None,
            flash: None,
        }
    }

    pub const fn is_open(&self) -> bool {
        self.input.is_some()
    }

    pub fn open(&mut self) {
        self.input = Some(String::new());
        self.flash = None;
    }

    pub fn flash(&mut self, flash: Flash) {
        self.flash = Some(flash);
    }

    /// Edit the open input, returning its text when submitted.
    /// Esc, or Backspace on empty input, closes without submitting.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        let input = self.input.as_mut()?;
        match key.code {
            KeyCode::Enter => return self.input.take(),
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                if input.pop().is_none() {
                    self.input = None;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
            _ => {}
        }
        None
    }

    fn block(&self) -> Block<'static> {
        panel::block("command", self.is_open())
    }

    pub fn cursor(&self, area: Rect) -> Option<Position> {
        let input = self.input.as_ref()?;
        let area = self.block().inner(area);
        let x = area.x.saturating_add(1 + input.chars().count() as u16);
        Some(Position::new(x.min(area.right().saturating_sub(1)), area.y))
    }
}

impl Widget for &Palette {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = match (&self.input, &self.flash) {
            (Some(input), _) => Line::from(vec![
                Span::styled(":", theme::KEY),
                Span::raw(input.as_str()),
            ]),
            (None, Some(Flash::Error(message))) => Line::styled(message.as_str(), theme::ERROR),
            (None, None) => Line::styled("type : for commands", theme::DIM),
        };
        let block = self.block();
        let inner = block.inner(area);
        block.render(area, buf);
        line.render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(palette: &mut Palette, code: KeyCode) -> Option<String> {
        palette.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn submits_typed_text() {
        let mut palette = Palette::new();
        assert_eq!(press(&mut palette, KeyCode::Char('q')), None);
        palette.open();
        press(&mut palette, KeyCode::Char('q'));
        press(&mut palette, KeyCode::Char('x'));
        press(&mut palette, KeyCode::Backspace);
        assert_eq!(press(&mut palette, KeyCode::Enter), Some("q".into()));
        assert!(!palette.is_open());
    }

    #[test]
    fn backspace_on_empty_closes() {
        let mut palette = Palette::new();
        palette.open();
        press(&mut palette, KeyCode::Backspace);
        assert!(!palette.is_open());
    }
}
