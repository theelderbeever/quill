//! The `:` command and `/` filter line, which otherwise shows status.
use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use super::{
    input::{InputEvent, TextInput},
    panel, theme,
};

/// Bordered, one line of text.
pub const HEIGHT: u16 = 3;

pub enum Flash {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Command,
    /// Narrows a list as you type.
    Filter,
    /// A SQL condition on the results, applied on Enter.
    Where,
}

impl Mode {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Command => ":",
            Self::Filter | Self::Where => "/",
        }
    }
}

pub enum PaletteEvent {
    None,
    Command(String),
    /// The filter text changed; apply it live.
    Filter(String),
    /// Esc on the filter: clear it.
    FilterCancelled,
    /// A results condition; empty clears it.
    Where(String),
}

pub struct Palette {
    open: Option<(Mode, TextInput)>,
    flash: Option<Flash>,
}

impl Palette {
    pub const fn new() -> Self {
        Self {
            open: None,
            flash: None,
        }
    }

    pub const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, mode: Mode) {
        self.open_with(mode, "");
    }

    pub fn open_with(&mut self, mode: Mode, text: &str) {
        self.open = Some((mode, TextInput::with_text(text)));
        self.flash = None;
    }

    pub fn flash(&mut self, flash: Flash) {
        self.flash = Some(flash);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PaletteEvent {
        let Some((mode, input)) = &mut self.open else {
            return PaletteEvent::None;
        };
        let mode = *mode;
        match (input.handle_key(key), mode) {
            (InputEvent::Submit, Mode::Command) => {
                let text = input.text().to_owned();
                self.open = None;
                PaletteEvent::Command(text)
            }
            (InputEvent::Submit, Mode::Where) => {
                let text = input.text().trim().to_owned();
                self.open = None;
                PaletteEvent::Where(text)
            }
            // The live filter is already applied; Enter just keeps it.
            (InputEvent::Submit, Mode::Filter) => {
                self.open = None;
                PaletteEvent::None
            }
            (InputEvent::Cancel, mode) => {
                self.open = None;
                match mode {
                    Mode::Command | Mode::Where => PaletteEvent::None,
                    Mode::Filter => PaletteEvent::FilterCancelled,
                }
            }
            (InputEvent::Edited, Mode::Filter) => PaletteEvent::Filter(input.text().to_owned()),
            (InputEvent::Edited | InputEvent::Ignored, _) => PaletteEvent::None,
        }
    }

    fn block(&self) -> Block<'static> {
        let title = match &self.open {
            Some((Mode::Filter, _)) => "filter",
            Some((Mode::Where, _)) => "where",
            _ => "command",
        };
        panel::block(title, self.is_open())
    }

    /// `status` (e.g. run progress) shows when the palette is closed, ahead
    /// of any message.
    pub fn render(&self, frame: &mut Frame, area: Rect, status: Option<Line<'_>>) {
        let block = self.block();
        let inner = block.inner(area);
        let line = match (&self.open, status, &self.flash) {
            (Some((mode, input)), _, _) => Line::from(vec![
                Span::styled(mode.prefix(), theme::KEY),
                Span::raw(input.text()),
            ]),
            (None, Some(status), _) => status,
            (None, None, Some(Flash::Error(message))) => {
                Line::styled(message.as_str(), theme::ERROR)
            }
            (None, None, Some(Flash::Info(message))) => Line::styled(message.as_str(), theme::OK),
            (None, None, None) => Line::styled(": command · / filter", theme::DIM),
        };
        frame.render_widget(Paragraph::new(line).block(block), area);
        if let Some((_, input)) = &self.open {
            let x = inner.x.saturating_add(1 + input.width());
            frame.set_cursor_position(Position::new(
                x.min(inner.right().saturating_sub(1)),
                inner.y,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn press(palette: &mut Palette, code: KeyCode) -> PaletteEvent {
        palette.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn submits_commands() {
        let mut palette = Palette::new();
        assert!(matches!(
            press(&mut palette, KeyCode::Char('q')),
            PaletteEvent::None
        ));
        palette.open(Mode::Command);
        press(&mut palette, KeyCode::Char('q'));
        press(&mut palette, KeyCode::Char('x'));
        press(&mut palette, KeyCode::Backspace);
        assert!(
            matches!(press(&mut palette, KeyCode::Enter), PaletteEvent::Command(c) if c == "q")
        );
        assert!(!palette.is_open());
    }

    #[test]
    fn filter_reports_each_edit_and_cancel() {
        let mut palette = Palette::new();
        palette.open(Mode::Filter);
        assert!(
            matches!(press(&mut palette, KeyCode::Char('r')), PaletteEvent::Filter(f) if f == "r")
        );
        assert!(matches!(
            press(&mut palette, KeyCode::Esc),
            PaletteEvent::FilterCancelled
        ));
        assert!(!palette.is_open());
    }
}
