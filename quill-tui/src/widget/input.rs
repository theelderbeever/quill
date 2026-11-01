//! A single-line text field.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum InputEvent {
    Submit,
    Cancel,
    Edited,
    Ignored,
}

#[derive(Default)]
pub struct TextInput {
    text: String,
}

impl TextInput {
    pub fn with_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Enter submits and Esc cancels; Backspace on empty input also cancels.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputEvent {
        match key.code {
            KeyCode::Enter => InputEvent::Submit,
            KeyCode::Esc => InputEvent::Cancel,
            KeyCode::Backspace => match self.text.pop() {
                Some(_) => InputEvent::Edited,
                None => InputEvent::Cancel,
            },
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.push(c);
                InputEvent::Edited
            }
            _ => InputEvent::Ignored,
        }
    }

    /// Cell width of the text, for placing the cursor after it.
    pub fn width(&self) -> u16 {
        u16::try_from(unicode_width::UnicodeWidthStr::width(self.text.as_str())).unwrap_or(u16::MAX)
    }
}
