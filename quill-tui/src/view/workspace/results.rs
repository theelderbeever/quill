//! The results panel: progress while running, then a paged grid that can be
//! sorted.
mod columns;
mod grid;
mod page;

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Paragraph, Wrap},
};

use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::error_chain,
    format,
    result::{Collected, ResultTable, Truncation},
    task::{RunId, derive, page::Page, stats},
    widget::{
        inspector::{self, Inspector, InspectorOutcome},
        palette::Flash,
        panel, theme,
    },
};
use columns::Widths;
use grid::{Grid, GridOutcome};

const GRID_HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "inspect"),
    KeyHint::new("y", "copy cell"),
    KeyHint::new("s", "sort"),
    KeyHint::new("-/+/=", "width"),
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    pub const fn arrow(self) -> &'static str {
        match self {
            Self::Ascending => "▲",
            Self::Descending => "▼",
        }
    }
}

/// The run's own result, which views are derived from.
struct Base {
    run: RunId,
    table: Arc<ResultTable>,
}

impl Base {
    fn new(run: RunId, table: ResultTable) -> Self {
        Self {
            run,
            table: Arc::new(table),
        }
    }
}

/// How the shown rows differ from the base result: a sort column, by
/// index and name, and its direction.
#[derive(Default)]
struct ViewSpec {
    sort: Option<(usize, String, Direction)>,
}

impl ViewSpec {
    fn describe(&self) -> Option<String> {
        self.sort
            .as_ref()
            .map(|(_, c, d)| format!("sort {c} {}", d.arrow()))
    }
}

struct Ready {
    base: Base,
    spec: ViewSpec,
    grid: Grid,
    /// View being collected; a result for any other is stale.
    pending: Option<RunId>,
}

enum State {
    Empty,
    Running,
    Message(String),
    Failed(String),
    Ready(Box<Ready>),
}

pub struct Results {
    state: State,
    /// Why the shown rows are incomplete.
    banner: Option<String>,
    /// Which result set is shown, as `(position, count)`, when there are several.
    set: Option<(usize, usize)>,
    widths: Widths,
    inspector: Option<Inspector>,
}

impl Results {
    pub fn new() -> Self {
        Self {
            state: State::Empty,
            banner: None,
            set: None,
            widths: Widths::default(),
            inspector: None,
        }
    }

    pub fn running(&mut self) {
        self.state = State::Running;
        self.banner = None;
        self.set = None;
        self.inspector = None;
    }

    /// Whether `run` is the shown result or the view drawn from it.
    pub fn owns(&self, run: RunId) -> bool {
        match &self.state {
            State::Ready(ready) => ready.base.run == run || ready.grid.run() == run,
            _ => false,
        }
    }

    pub const fn has_grid(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    /// Show a finished run and start profiling it. The last result set
    /// holding rows is shown.
    pub fn finish(&mut self, run: RunId, result: anyhow::Result<Collected>, ctx: &Context) {
        let stored = match result {
            Ok(stored) => stored,
            Err(e) => {
                self.state = State::Failed(format!("{e:#}"));
                return;
            }
        };
        let count = stored.result_sets.len();
        let affected: u64 = stored
            .result_sets
            .iter()
            .filter_map(|s| s.affected_rows)
            .sum();
        let shown = stored
            .result_sets
            .into_iter()
            .enumerate()
            .rev()
            .find_map(|(i, set)| set.data.map(|data| (i, data)));
        self.banner = stored.truncation.as_ref().map(|t| match t {
            Truncation::Cancelled => "cancelled; showing rows received before cancelling".into(),
            Truncation::Failed(e) => format!(
                "failed: {}; showing rows received before the error",
                error_chain(e)
            ),
        });
        let previous = std::mem::replace(&mut self.state, State::Running);
        self.state = match (shown, stored.truncation) {
            (Some((index, data)), _) => {
                self.set = (count > 1).then_some((index + 1, count));
                let mut ready = Self::ready(run, data, ctx);
                if let State::Ready(old) = &previous {
                    ready.grid.follow(&old.grid);
                }
                State::Ready(Box::new(ready))
            }
            (None, Some(Truncation::Failed(e))) => {
                self.banner = None;
                State::Failed(error_chain(&e))
            }
            (None, _) => State::Message(format!(
                "Statement complete. {} rows affected.",
                format::count(affected)
            )),
        };
    }

    fn ready(run: RunId, data: ResultTable, ctx: &Context) -> Ready {
        let base = Base::new(run, data);
        stats::spawn_profile(base.table.clone(), run, ctx.shared.messages.clone());
        let page_rows = ctx.options().page_rows.get();
        let grid = Grid::new(run, base.table.clone(), page_rows);
        Ready {
            base,
            spec: ViewSpec::default(),
            grid,
            pending: None,
        }
    }

    /// Re-derive the shown rows after the view spec changed.
    fn derive(ready: &mut Ready, ctx: &Context) {
        let Some((column, _, direction)) = &ready.spec.sort else {
            let page_rows = ctx.options().page_rows.get();
            let grid = Grid::new(ready.base.run, ready.base.table.clone(), page_rows);
            let previous = std::mem::replace(&mut ready.grid, grid);
            ready.grid.follow(&previous);
            ready.pending = None;
            return;
        };
        let view = RunId::next();
        ready.pending = Some(view);
        derive::spawn_view(
            ready.base.table.clone(),
            *column,
            *direction == Direction::Descending,
            ready.base.run,
            view,
            ctx.shared.messages.clone(),
        );
    }

    /// Ascending, then descending, then unsorted, on the cursor's column.
    fn cycle_sort(&mut self, ctx: &Context) {
        let State::Ready(ready) = &mut self.state else {
            return;
        };
        let Some((index, column)) = ready.grid.column() else {
            return;
        };
        let column = column.to_owned();
        ready.spec.sort = match ready.spec.sort.take() {
            Some((i, c, Direction::Ascending)) if i == index => Some((i, c, Direction::Descending)),
            Some((i, _, Direction::Descending)) if i == index => None,
            _ => Some((index, column, Direction::Ascending)),
        };
        Self::derive(ready, ctx);
    }

    /// A sorted view finished.
    pub fn derived(
        &mut self,
        base: RunId,
        view: RunId,
        result: anyhow::Result<ResultTable>,
        ctx: &Context,
    ) -> Option<Flash> {
        let State::Ready(ready) = &mut self.state else {
            return None;
        };
        if ready.base.run != base || ready.pending != Some(view) {
            return None;
        }
        ready.pending = None;
        let table = match result {
            Ok(table) => table,
            Err(e) => return Some(Flash::Error(format!("{e:#}"))),
        };
        let page_rows = ctx.options().page_rows.get();
        let grid = Grid::new(view, Arc::new(table), page_rows);
        let previous = std::mem::replace(&mut ready.grid, grid);
        ready.grid.follow(&previous);
        None
    }

    /// Store a fetched page. Returns an error message to report, if any.
    pub fn page(&mut self, run: RunId, index: u64, result: anyhow::Result<Page>) -> Option<Flash> {
        let State::Ready(ready) = &mut self.state else {
            return None;
        };
        if ready.grid.run() != run {
            return None;
        }
        match result {
            Ok(page) => {
                ready.grid.insert_page(index, page);
                None
            }
            Err(e) => Some(Flash::Error(format!("loading rows failed: {e:#}"))),
        }
    }

    /// The cell inspector, drawn over the whole workspace.
    pub fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        if let Some(inspector) = &self.inspector {
            inspector.render(frame, area);
        }
    }

    pub fn handle_overlay_mouse(&mut self, event: MouseEvent) -> bool {
        let Some(inspector) = &self.inspector else {
            return false;
        };
        if matches!(event.kind, crossterm::event::MouseEventKind::Down(_))
            && !inspector.contains(event.column, event.row)
        {
            self.inspector = None;
        }
        true
    }

    fn title(&self) -> String {
        let mut title = "results".to_owned();
        if let State::Ready(ready) = &self.state {
            title.push_str(&format!(" · {} rows", format::count(ready.grid.rows())));
            if let Some(spec) = ready.spec.describe() {
                title.push_str(&format!(" · {spec}"));
            }
            if ready.pending.is_some() {
                title.push_str(" · updating…");
            }
        }
        if let Some((position, count)) = self.set {
            title.push_str(&format!(" · set {position} of {count}"));
        }
        title
    }
}

impl Component for Results {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        if let Some(inspector) = &mut self.inspector {
            return match inspector.handle_key(key) {
                InspectorOutcome::Open => KeyOutcome::Consumed,
                InspectorOutcome::Close => {
                    self.inspector = None;
                    KeyOutcome::Consumed
                }
                InspectorOutcome::Copy(text) => KeyOutcome::Action(Action::Copy(text)),
            };
        }
        if key.code == KeyCode::Char('y') {
            return self.copy_text().map_or(KeyOutcome::Ignored, |text| {
                KeyOutcome::Action(Action::Copy(text))
            });
        }
        if key.code == KeyCode::Char('s') && self.has_grid() {
            self.cycle_sort(ctx);
            return KeyOutcome::Consumed;
        }
        let State::Ready(ready) = &mut self.state else {
            return KeyOutcome::Ignored;
        };
        match ready.grid.handle_key(key, &mut self.widths) {
            GridOutcome::Consumed => KeyOutcome::Consumed,
            GridOutcome::Ignored => KeyOutcome::Ignored,
            GridOutcome::Inspect(inspector) => {
                self.inspector = Some(inspector);
                KeyOutcome::Consumed
            }
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent, _ctx: &Context) -> KeyOutcome {
        if let State::Ready(ready) = &mut self.state {
            ready.grid.handle_mouse(event, &mut self.widths);
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let block = panel::block(&self.title(), ctx.focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [banner_area, body] = Layout::vertical([
            Constraint::Length(u16::from(self.banner.is_some())),
            Constraint::Min(0),
        ])
        .areas(inner);
        if let Some(banner) = &self.banner {
            frame.render_widget(Line::styled(banner.as_str(), theme::BANNER), banner_area);
        }
        let message = |text: &str, style| Paragraph::new(text.to_owned()).style(style);
        match &mut self.state {
            State::Empty => frame.render_widget(
                message("Run a query with F5 or ctrl-enter.", theme::DIM),
                body,
            ),
            State::Running => frame.render_widget(message("Running…", theme::PENDING), body),
            State::Message(text) => frame.render_widget(message(text, theme::DIM), body),
            State::Failed(error) => frame.render_widget(
                message(error, theme::ERROR).wrap(Wrap { trim: false }),
                body,
            ),
            State::Ready(ready) => {
                let sort = ready.spec.sort.as_ref().map(|(i, _, d)| (*i, *d));
                ready.grid.render(frame, body, ctx, &self.widths, sort);
            }
        }
    }

    fn hints(&self) -> &'static [KeyHint] {
        match (&self.inspector, &self.state) {
            (Some(_), _) => inspector::HINTS,
            (None, State::Ready(_)) => GRID_HINTS,
            (None, _) => &[],
        }
    }

    fn captures_input(&self) -> bool {
        self.inspector.is_some()
    }

    /// The inspected value, else the cell under the cursor.
    fn copy_text(&mut self) -> Option<String> {
        if let Some(inspector) = &self.inspector {
            return Some(inspector.text().to_owned());
        }
        match &self.state {
            State::Ready(ready) => ready.grid.cell().map(str::to_owned),
            _ => None,
        }
    }
}
