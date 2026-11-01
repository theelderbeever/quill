//! Statistics for the last run: client, sink and server metrics, then a
//! profile of each column.
use crossterm::event::{KeyCode, KeyEvent};
use quill_core::query::{ClientMetrics, ServerMetrics};
use quill_io::feather::SinkMetrics;
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::error_chain,
    format,
    task::{RunId, stats::Profile},
    widget::{panel, theme},
};

const HINTS: &[KeyHint] = &[KeyHint::new("hjkl", "scroll")];

enum Pending<T> {
    Waiting,
    Ready(T),
    Failed(String),
}

struct RunStats {
    run: RunId,
    client: Option<ClientMetrics>,
    sink: Option<SinkMetrics>,
    server: Pending<Option<ServerMetrics>>,
    profile: Pending<Profile>,
}

pub struct Stats {
    run: Option<RunStats>,
    /// Lines and columns scrolled past; lines are never wrapped.
    scroll: (u16, u16),
}

impl Stats {
    pub const fn new() -> Self {
        Self {
            run: None,
            scroll: (0, 0),
        }
    }

    pub fn started(&mut self, run: RunId) {
        self.run = Some(RunStats {
            run,
            client: None,
            sink: None,
            server: Pending::Waiting,
            profile: Pending::Waiting,
        });
        self.scroll = (0, 0);
    }

    fn current(&mut self, run: RunId) -> Option<&mut RunStats> {
        self.run.as_mut().filter(|r| r.run == run)
    }

    pub fn finished(
        &mut self,
        run: RunId,
        client: Option<ClientMetrics>,
        sink: SinkMetrics,
        server: bool,
    ) {
        if let Some(stats) = self.current(run) {
            stats.client = client;
            stats.sink = Some(sink);
            if !server {
                stats.server = Pending::Ready(None);
            }
        }
    }

    pub fn profile(&mut self, run: RunId, result: anyhow::Result<Profile>) {
        if let Some(stats) = self.current(run) {
            stats.profile = match result {
                Ok(profile) => Pending::Ready(profile),
                Err(e) => Pending::Failed(format!("{e:#}")),
            };
        }
    }

    pub fn server(&mut self, run: RunId, result: quill_core::error::Result<Option<ServerMetrics>>) {
        if let Some(stats) = self.current(run) {
            stats.server = match result {
                Ok(metrics) => Pending::Ready(metrics),
                Err(e) => Pending::Failed(error_chain(&e)),
            };
        }
    }

    fn lines(stats: &RunStats) -> Vec<Line<'static>> {
        let heading = |text: &'static str| Line::styled(text, theme::TITLE);
        let item = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!(" {label} "), theme::DIM),
                Span::raw(value),
            ])
        };
        let mut lines = vec![heading("run")];
        match &stats.client {
            Some(client) => {
                lines.push(item("elapsed", format::duration(client.elapsed)));
                if let Some(first) = client.first_row {
                    lines.push(item("first row", format::duration(first)));
                }
                lines.push(item(
                    "delivered",
                    format!(
                        "{} rows · {} batches",
                        format::count(client.delivered_rows),
                        format::count(client.delivered_batches)
                    ),
                ));
                if let Some(affected) = client.affected_rows {
                    lines.push(item("affected", format::count(affected)));
                }
            }
            None => lines.push(Line::styled(" running…", theme::PENDING)),
        }
        if let Some(sink) = &stats.sink {
            lines.push(item(
                "stored",
                format!(
                    "{} rows · {}",
                    format::count(sink.rows_written),
                    format::bytes(sink.bytes_written)
                ),
            ));
        }

        lines.push(Line::raw(""));
        lines.push(heading("server"));
        match &stats.server {
            Pending::Waiting => lines.push(Line::styled(" waiting…", theme::DIM)),
            Pending::Failed(error) => lines.push(Line::styled(format!(" {error}"), theme::ERROR)),
            Pending::Ready(None) => lines.push(Line::styled(" none recorded", theme::DIM)),
            Pending::Ready(Some(server)) => {
                let rows_bytes = |rows: Option<u64>, bytes: Option<u64>| match (rows, bytes) {
                    (None, None) => None,
                    (rows, bytes) => Some(
                        [
                            rows.map(|r| format!("{} rows", format::count(r))),
                            bytes.map(format::bytes),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · "),
                    ),
                };
                let entries = [
                    ("elapsed", server.elapsed.map(format::duration)),
                    ("read", rows_bytes(server.read_rows, server.read_bytes)),
                    (
                        "written",
                        rows_bytes(server.written_rows, server.written_bytes),
                    ),
                    (
                        "result",
                        rows_bytes(server.result_rows, server.result_bytes),
                    ),
                    ("peak memory", server.peak_memory_bytes.map(format::bytes)),
                    ("spilled", server.spilled_bytes.map(format::bytes)),
                    ("compute", server.compute_time.map(format::duration)),
                ];
                for (label, value) in entries {
                    if let Some(value) = value {
                        lines.push(item(label, value));
                    }
                }
            }
        }

        lines.push(Line::raw(""));
        lines.push(heading("columns"));
        match &stats.profile {
            Pending::Waiting => lines.push(Line::styled(" profiling…", theme::DIM)),
            Pending::Failed(error) => lines.push(Line::styled(format!(" {error}"), theme::ERROR)),
            Pending::Ready(profile) => {
                lines.push(item(
                    "scanned",
                    format!("{} rows", format::count(profile.rows)),
                ));
                for column in &profile.columns {
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {}", column.name), theme::KEY),
                        Span::styled(format!(" {}", column.data_type), theme::DIM),
                    ]));
                    lines.push(item("  nulls", format::count(column.nulls)));
                    if let Some(min) = &column.min {
                        lines.push(item("  min", min.clone()));
                    }
                    if let Some(max) = &column.max {
                        lines.push(item("  max", max.clone()));
                    }
                }
            }
        }
        lines
    }
}

impl Component for Stats {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll.0 = self.scroll.0.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll.0 = self.scroll.0.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right => self.scroll.1 = self.scroll.1.saturating_add(2),
            KeyCode::Char('h') | KeyCode::Left => self.scroll.1 = self.scroll.1.saturating_sub(2),
            KeyCode::Char('g') => self.scroll = (0, 0),
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let block = panel::block("stats", ctx.focused);
        let paragraph = match &self.run {
            Some(stats) => Paragraph::new(Self::lines(stats)),
            None => Paragraph::new("Statistics appear after a run.").style(theme::DIM),
        };
        frame.render_widget(paragraph.scroll(self.scroll).block(block), area);
    }

    fn hints(&self) -> &'static [KeyHint] {
        HINTS
    }
}
