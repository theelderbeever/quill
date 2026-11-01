//! Saved queries for one connection.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{List, ListState, Paragraph},
};

use super::Route;
use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionId,
    filter::Filter,
    store::queries::{QueryName, QueryStore},
    widget::{
        input::{InputEvent, TextInput},
        panel, prompt, theme,
    },
};

const HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "open"),
    KeyHint::new("a", "add"),
    KeyHint::new("e", "rename"),
    KeyHint::new("d", "delete"),
];
const PROMPT_HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "confirm"),
    KeyHint::new("esc", "cancel"),
];

enum Prompt {
    Add(TextInput),
    Rename(QueryName, TextInput),
    Delete(QueryName),
}

pub struct QueriesView {
    connection: ConnectionId,
    store: QueryStore,
    names: Vec<QueryName>,
    filter: Option<Filter>,
    /// Index into the visible names.
    selected: usize,
    list: ListState,
    prompt: Option<Prompt>,
}

impl QueriesView {
    pub fn open(connection: ConnectionId, ctx: &Context) -> anyhow::Result<Self> {
        let store = QueryStore::new(ctx.connections().query_dir(&connection));
        let names = store.list()?;
        Ok(Self {
            connection,
            store,
            names,
            filter: None,
            selected: 0,
            list: ListState::default(),
            prompt: None,
        })
    }

    pub const fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    pub fn set_filter(&mut self, filter: Option<Filter>) {
        self.filter = filter;
        self.selected = 0;
    }

    fn visible(&self) -> impl Iterator<Item = &QueryName> {
        self.names
            .iter()
            .filter(|n| self.filter.as_ref().is_none_or(|f| f.matches(n.as_str())))
    }

    fn selected_name(&self) -> Option<QueryName> {
        self.visible().nth(self.selected).cloned()
    }

    /// Reload from disk and select `name` if present.
    fn reload(&mut self, select: Option<&QueryName>) -> anyhow::Result<()> {
        self.names = self.store.list()?;
        if let Some(index) = select.and_then(|s| self.visible().position(|n| n == s)) {
            self.selected = index;
        }
        Ok(())
    }

    fn handle_prompt(&mut self, key: KeyEvent) -> anyhow::Result<()> {
        let Some(prompt) = &mut self.prompt else {
            return Ok(());
        };
        let submitted = match prompt {
            Prompt::Add(input) | Prompt::Rename(_, input) => match input.handle_key(key) {
                InputEvent::Submit => true,
                InputEvent::Cancel => {
                    self.prompt = None;
                    return Ok(());
                }
                InputEvent::Edited | InputEvent::Ignored => return Ok(()),
            },
            Prompt::Delete(_) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => true,
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.prompt = None;
                    return Ok(());
                }
                _ => return Ok(()),
            },
        };
        if !submitted {
            return Ok(());
        }
        // Keep the prompt open on invalid input so it can be corrected.
        let select = match prompt {
            Prompt::Add(input) => {
                let name = QueryName::parse(input.text())?;
                self.store.create(&name)?;
                Some(name)
            }
            Prompt::Rename(from, input) => {
                let to = QueryName::parse(input.text())?;
                if *from != to {
                    self.store.rename(from, &to)?;
                }
                Some(to)
            }
            Prompt::Delete(name) => {
                self.store.delete(name)?;
                None
            }
        };
        self.prompt = None;
        self.reload(select.as_ref())
    }
}

impl Component for QueriesView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        if self.prompt.is_some() {
            return match self.handle_prompt(key) {
                Ok(()) => KeyOutcome::Consumed,
                Err(e) => KeyOutcome::Action(Action::Report(format!("{e:#}"))),
            };
        }
        let count = self.visible().count();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('a') => self.prompt = Some(Prompt::Add(TextInput::default())),
            KeyCode::Char('e') => {
                if let Some(name) = self.selected_name() {
                    let input = TextInput::with_text(name.as_str());
                    self.prompt = Some(Prompt::Rename(name, input));
                }
            }
            KeyCode::Char('d') => {
                if let Some(name) = self.selected_name() {
                    self.prompt = Some(Prompt::Delete(name));
                }
            }
            KeyCode::Enter => {
                if let Some(query) = self.selected_name() {
                    return KeyOutcome::Action(Action::Open(Route::Workspace {
                        connection: self.connection.clone(),
                        query,
                    }));
                }
            }
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let count = self.visible().count();
        self.selected = self.selected.min(count.saturating_sub(1));
        let mut title = format!("{} queries ({count})", self.connection);
        if let Some(filter) = &self.filter {
            title = format!("{title} {}", filter.describe());
        }
        let block = panel::block(&title, ctx.focused && self.prompt.is_none());
        if count == 0 {
            let message = if self.names.is_empty() {
                format!(
                    "No saved queries in {}. Press a to add one.",
                    self.store.dir().display()
                )
            } else {
                "No queries match the filter.".to_owned()
            };
            frame.render_widget(Paragraph::new(message).style(theme::DIM).block(block), area);
        } else {
            let filter = self.filter.as_ref();
            let names = self
                .names
                .iter()
                .filter(|n| filter.is_none_or(|f| f.matches(n.as_str())))
                .map(QueryName::as_str);
            let list = List::new(names)
                .highlight_style(theme::SELECTED)
                .block(block);
            self.list.select(Some(self.selected));
            frame.render_stateful_widget(list, area, &mut self.list);
        }

        match &self.prompt {
            Some(Prompt::Add(input)) => prompt::render_input(frame, area, "new query", input),
            Some(Prompt::Rename(name, input)) => {
                prompt::render_input(frame, area, &format!("rename {name}"), input);
            }
            Some(Prompt::Delete(name)) => {
                prompt::render_confirm(frame, area, "delete", &format!("Delete {name}.sql?"));
            }
            None => {}
        }
    }

    fn hints(&self) -> &'static [KeyHint] {
        if self.prompt.is_some() {
            PROMPT_HINTS
        } else {
            HINTS
        }
    }

    fn captures_input(&self) -> bool {
        self.prompt.is_some()
    }
}
