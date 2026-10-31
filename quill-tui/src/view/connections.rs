//! Connections defined in config.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::Span,
    widgets::{Paragraph, Row, Table, TableState},
};

use super::Route;
use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionState,
    widget::{panel, theme},
};

const HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "open"),
    KeyHint::new("x", "disconnect"),
    KeyHint::new("j/k", "move"),
];
const NAME_WIDTH: u16 = 24;
const KIND_WIDTH: u16 = 12;

pub struct ConnectionsView {
    table: TableState,
}

impl ConnectionsView {
    pub fn new() -> Self {
        Self {
            table: TableState::new().with_selected(0),
        }
    }
}

impl Component for ConnectionsView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        let last = ctx.connections.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.table
                    .select(self.table.selected().map(|i| (i + 1).min(last)));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.table
                    .select(self.table.selected().map(|i| i.saturating_sub(1)));
            }
            KeyCode::Enter | KeyCode::Char('x') => {
                let selected = self
                    .table
                    .selected()
                    .and_then(|i| ctx.connections.get_index(i));
                if let Some(name) = selected.cloned() {
                    return KeyOutcome::Action(match key.code {
                        KeyCode::Enter => Action::Open(Route::Queries(name)),
                        _ => Action::Disconnect(name),
                    });
                }
            }
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let block = panel::block(
            &format!("connections ({})", ctx.connections.len()),
            ctx.focused,
        );
        if ctx.connections.is_empty() {
            let empty = Paragraph::new("No connections in config.").style(theme::DIM);
            frame.render_widget(empty.block(block), area);
            return;
        }
        let rows = ctx.connections.iter().map(|(name, kind, state)| {
            let style = match state {
                ConnectionState::Idle => theme::DIM,
                ConnectionState::Connecting => theme::PENDING,
                ConnectionState::Connected(_) => theme::OK,
                ConnectionState::Failed(_) => theme::ERROR,
            };
            Row::new([
                Span::raw(name.as_str()),
                Span::raw(kind),
                Span::styled(state.label(), style),
            ])
        });
        let table = Table::new(
            rows,
            [
                Constraint::Length(NAME_WIDTH),
                Constraint::Length(KIND_WIDTH),
                Constraint::Min(0),
            ],
        )
        .header(Row::new(["NAME", "KIND", "STATUS"]).style(theme::TITLE))
        .row_highlight_style(theme::SELECTED)
        .block(block);
        frame.render_stateful_widget(table, area, &mut self.table);
    }

    fn hints(&self) -> &'static [KeyHint] {
        HINTS
    }
}
