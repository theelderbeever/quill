//! Connection details, the focused component's hotkeys, and the logo.
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{logo, theme};
use crate::{
    component::KeyHint,
    connection::{ConnectionState, Connections},
    view::View,
};

pub const HEIGHT: u16 = logo::LOGO.len() as u16;
const DETAILS_WIDTH: u16 = 36;
const HINT_GAP: u16 = 3;

pub struct Header<'a> {
    pub connections: &'a Connections,
    pub view: &'a View,
    /// Groups shown in order: focused component, view, then global keys.
    pub hints: [&'static [KeyHint]; 3],
}

impl Header<'_> {
    fn details(&self) -> Vec<Line<'_>> {
        let breadcrumb = self
            .view
            .breadcrumb()
            .map(Span::raw)
            .flat_map(|crumb| [Span::styled(" › ", theme::DIM), crumb])
            .skip(1);
        let Some(id) = self.view.connection() else {
            return vec![
                Line::styled("quill", theme::TITLE),
                Line::styled(
                    format!("{} connections", self.connections.len()),
                    theme::DIM,
                ),
                Line::from("connections"),
            ];
        };
        let status = self.connections.state(id).map_or(Span::raw(""), |state| {
            let style = match state {
                ConnectionState::Idle => theme::DIM,
                ConnectionState::Connecting => theme::PENDING,
                ConnectionState::Connected(_) => theme::OK,
                ConnectionState::Failed(_) => theme::ERROR,
            };
            Span::styled(state.label(), style)
        });
        vec![
            Line::from(vec![
                Span::styled(id.name(), theme::TITLE),
                Span::styled(" · ", theme::DIM),
                Span::raw(self.connections.kind(id)),
            ]),
            Line::from(status),
            Line::from_iter(breadcrumb),
            Line::styled(self.connections.label(id), theme::DIM),
        ]
    }

    /// Lay hints out top to bottom in columns, k9s style.
    fn render_hints(&self, area: Rect, buf: &mut Buffer) {
        let hints: Vec<&KeyHint> = self.hints.iter().flat_map(|group| group.iter()).collect();
        let mut x = area.x;
        for column in hints.chunks(usize::from(area.height.max(1))) {
            let key_width = column.iter().map(|h| h.key.len()).max().unwrap_or(0);
            let width = column
                .iter()
                .map(|h| key_width + h.action.len() + 3)
                .max()
                .unwrap_or(0) as u16;
            let lines = column.iter().map(|h| {
                Line::from(vec![
                    Span::styled(format!("<{}>", h.key), theme::KEY),
                    Span::raw(" ".repeat(key_width - h.key.len() + 1)),
                    Span::styled(h.action, theme::DIM),
                ])
            });
            let right = area.right();
            if x >= right {
                break;
            }
            let column_area = Rect::new(x, area.y, width.min(right - x), area.height);
            Paragraph::new(Vec::from_iter(lines)).render(column_area, buf);
            x = x.saturating_add(width + HINT_GAP);
        }
    }
}

impl Widget for Header<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [details, hints, logo_area] = Layout::horizontal([
            Constraint::Length(DETAILS_WIDTH),
            Constraint::Min(0),
            Constraint::Length(logo::WIDTH),
        ])
        .areas(area);
        Paragraph::new(self.details()).render(details, buf);
        self.render_hints(hints, buf);
        Paragraph::new(Vec::from_iter(logo::LOGO.map(Line::raw)))
            .style(theme::LOGO)
            .render(logo_area, buf);
    }
}
