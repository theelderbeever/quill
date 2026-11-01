//! Connections from config, grouped under the file that defines them.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Row, Table, TableState},
};

use super::Route;
use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::{ConnectionId, ConnectionState, Connections},
    filter::Filter,
    widget::{panel, theme},
};

const HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "open"),
    KeyHint::new("x", "disconnect"),
    KeyHint::new("j/k", "move"),
];
const NAME_WIDTH: usize = 24;
const KIND_WIDTH: usize = 12;

pub struct ConnectionsView {
    filter: Option<Filter>,
    /// Index into the visible connections, not table rows.
    selected: usize,
    table: TableState,
}

impl ConnectionsView {
    pub fn new(filter: Option<Filter>) -> Self {
        Self {
            filter,
            selected: 0,
            table: TableState::new(),
        }
    }

    pub fn set_filter(&mut self, filter: Option<Filter>) {
        self.filter = filter;
        self.selected = 0;
    }

    fn visible<'a>(
        &'a self,
        connections: &'a Connections,
    ) -> impl Iterator<Item = (&'a ConnectionId, &'static str, &'a ConnectionState)> {
        connections
            .iter()
            .filter(|(id, ..)| self.filter.as_ref().is_none_or(|f| f.matches(id.name())))
    }
}

impl Component for ConnectionsView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        let count = self.visible(ctx.connections()).count();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('x') => {
                let selected = self.visible(ctx.connections()).nth(self.selected);
                if let Some((id, ..)) = selected {
                    let id = id.clone();
                    return KeyOutcome::Action(match key.code {
                        KeyCode::Enter => Action::Open(Route::Queries(id)),
                        _ => Action::Disconnect(id),
                    });
                }
            }
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let connections = ctx.connections();
        let count = self.visible(connections).count();
        self.selected = self.selected.min(count.saturating_sub(1));
        let title = match &self.filter {
            Some(filter) => format!("connections ({count}) {}", filter.describe()),
            None => format!("connections ({count})"),
        };
        let block = panel::block(&title, ctx.focused);
        if count == 0 {
            let message = if connections.is_empty() {
                "No connections in config."
            } else {
                "No connections match the filter."
            };
            frame.render_widget(Paragraph::new(message).style(theme::DIM).block(block), area);
            return;
        }

        let mut rows = Vec::new();
        let mut group = None;
        let mut selected_row = 0;
        for (index, (id, kind, state)) in self.visible(connections).enumerate() {
            let label = connections.label(id);
            if group != Some(label) {
                group = Some(label);
                rows.push(Row::new([Span::styled(label, theme::GROUP)]));
            }
            if index == self.selected {
                selected_row = rows.len();
            }
            let style = match state {
                ConnectionState::Idle => theme::DIM,
                ConnectionState::Connecting => theme::PENDING,
                ConnectionState::Connected(_) => theme::OK,
                ConnectionState::Failed(_) => theme::ERROR,
            };
            rows.push(Row::new([Line::from(vec![
                Span::raw(format!("  {:<name$} ", id.name(), name = NAME_WIDTH - 2)),
                Span::raw(format!("{kind:<KIND_WIDTH$} ")),
                Span::styled(state.label(), style),
            ])]));
        }
        self.table.select(Some(selected_row));
        // One full-width column, so group headings show the whole path.
        let header = format!("{:<NAME_WIDTH$} {:<KIND_WIDTH$} STATUS", "NAME", "KIND");
        let table = Table::new(rows, [Constraint::Min(0)])
            .header(Row::new([header]).style(theme::TITLE))
            .row_highlight_style(theme::SELECTED)
            .block(block);
        frame.render_stateful_widget(table, area, &mut self.table);
    }

    fn hints(&self) -> &'static [KeyHint] {
        HINTS
    }
}
