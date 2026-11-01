//! The results panel: progress while running, then a paged grid that can be
//! sorted, filtered and saved.
mod columns;
mod grid;
mod inspector;
mod page;

use std::{path::Path, sync::Arc};

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use datafusion::prelude::SessionContext;
use quill_io::feather::{FeatherTable, ROW_COLUMN, StoredQuery, StoredResult, Truncation};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Paragraph, Wrap},
};

use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::{error_chain, quote},
    format,
    task::{RunId, derive, page::Page, run::ResultDir, stats},
    widget::{palette::Flash, panel, theme},
};
use columns::Widths;
use grid::{Grid, GridOutcome};
use inspector::{Inspector, InspectorOutcome};

const GRID_HINTS: &[KeyHint] = &[
    KeyHint::new("hjkl", "move"),
    KeyHint::new("enter", "inspect"),
    KeyHint::new("y", "copy cell"),
    KeyHint::new("s", "sort"),
    KeyHint::new("/", "where"),
    KeyHint::new("-/+/=", "width"),
];
/// Name the run's own result is registered under for views and profiling.
const BASE: &str = "base";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    const fn sql(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }

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
    result: Arc<StoredResult>,
    ctx: SessionContext,
    columns: Vec<String>,
    dir: ResultDir,
}

impl Base {
    fn new(run: RunId, result: StoredResult, dir: ResultDir) -> anyhow::Result<Self> {
        let result = Arc::new(result);
        let columns = result
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        let ctx = SessionContext::new();
        ctx.register_table(BASE, Arc::new(FeatherTable::new(result.clone())?))?;
        Ok(Self {
            run,
            result,
            ctx,
            columns,
            dir,
        })
    }

    fn select_list(&self) -> String {
        let quoted: Vec<String> = self.columns.iter().map(|c| quote(c, '"')).collect();
        quoted.join(", ")
    }
}

/// How the shown rows differ from the base result.
#[derive(Default)]
struct ViewSpec {
    filter: Option<String>,
    sort: Option<(String, Direction)>,
}

impl ViewSpec {
    const fn is_plain(&self) -> bool {
        self.filter.is_none() && self.sort.is_none()
    }

    fn describe(&self) -> Option<String> {
        let parts: Vec<String> = self
            .filter
            .iter()
            .map(|f| format!("where {f}"))
            .chain(
                self.sort
                    .iter()
                    .map(|(c, d)| format!("sort {c} {}", d.arrow())),
            )
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

struct Ready {
    base: Base,
    spec: ViewSpec,
    grid: Grid,
    /// View being drained; a result for any other is stale.
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

    pub const fn has_grid(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    /// The current results condition, to edit in the palette.
    pub fn filter(&self) -> &str {
        match &self.state {
            State::Ready(ready) => ready.spec.filter.as_deref().unwrap_or(""),
            _ => "",
        }
    }

    /// Show a finished run and start profiling it. The last result set
    /// holding rows is shown.
    pub fn finish(
        &mut self,
        run: RunId,
        dir: &Path,
        result: anyhow::Result<StoredQuery>,
        ctx: &Context,
    ) {
        let dir = ResultDir::new(dir.to_path_buf());
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
            Truncation::ByteLimit => {
                "stopped at the results byte limit; showing rows written so far".into()
            }
            Truncation::Failed(e) => format!(
                "failed: {}; showing rows received before the error",
                error_chain(e)
            ),
        });
        let previous = std::mem::replace(&mut self.state, State::Running);
        self.state = match (shown, stored.truncation) {
            (Some((index, data)), _) => {
                self.set = (count > 1).then_some((index + 1, count));
                match Self::ready(run, data, dir, ctx) {
                    Ok(mut ready) => {
                        if let State::Ready(old) = &previous {
                            ready.grid.follow(&old.grid);
                        }
                        State::Ready(Box::new(ready))
                    }
                    Err(e) => State::Failed(format!("{e:#}")),
                }
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

    fn ready(
        run: RunId,
        data: StoredResult,
        dir: ResultDir,
        ctx: &Context,
    ) -> anyhow::Result<Ready> {
        let base = Base::new(run, data, dir)?;
        let schema = base.result.schema().clone();
        stats::spawn_profile(
            base.ctx.clone(),
            BASE,
            schema,
            run,
            ctx.shared.messages.clone(),
        );
        let page_rows = ctx.options().page_rows.get();
        let grid = Grid::new(run, base.result.clone(), None, page_rows)?;
        Ok(Ready {
            base,
            spec: ViewSpec::default(),
            grid,
            pending: None,
        })
    }

    /// Re-derive the shown rows after the view spec changed.
    fn derive(ready: &mut Ready, ctx: &Context) -> anyhow::Result<()> {
        if ready.spec.is_plain() {
            let page_rows = ctx.options().page_rows.get();
            let grid = Grid::new(ready.base.run, ready.base.result.clone(), None, page_rows)?;
            let previous = std::mem::replace(&mut ready.grid, grid);
            ready.grid.follow(&previous);
            ready.pending = None;
            return Ok(());
        }
        let mut sql = format!("SELECT {} FROM {BASE}", ready.base.select_list());
        if let Some(filter) = &ready.spec.filter {
            sql.push_str(&format!(" WHERE {filter}"));
        }
        match &ready.spec.sort {
            Some((column, direction)) => {
                sql.push_str(&format!(
                    " ORDER BY {} {}",
                    quote(column, '"'),
                    direction.sql()
                ));
            }
            // Keep the original order for a filter alone.
            None => sql.push_str(&format!(" ORDER BY {ROW_COLUMN}")),
        }
        let view = RunId::next();
        ready.pending = Some(view);
        let dir = ready.base.dir.path().join(format!("view-{view}"));
        let messages = ctx.shared.messages.clone();
        derive::spawn_view(
            ready.base.ctx.clone(),
            sql,
            dir,
            ready.base.run,
            view,
            messages,
        );
        Ok(())
    }

    /// Apply a SQL condition to the shown rows; empty clears it.
    pub fn set_filter(&mut self, filter: String, ctx: &Context) -> Option<Flash> {
        let State::Ready(ready) = &mut self.state else {
            return Some(Flash::Error("no results to filter".into()));
        };
        ready.spec.filter = (!filter.is_empty()).then_some(filter);
        Self::derive(ready, ctx)
            .err()
            .map(|e| Flash::Error(format!("{e:#}")))
    }

    /// Ascending, then descending, then unsorted, on the cursor's column.
    fn cycle_sort(&mut self, ctx: &Context) -> anyhow::Result<()> {
        let State::Ready(ready) = &mut self.state else {
            return Ok(());
        };
        let Some(column) = ready.grid.column().map(str::to_owned) else {
            return Ok(());
        };
        ready.spec.sort = match ready.spec.sort.take() {
            Some((c, Direction::Ascending)) if c == column => Some((c, Direction::Descending)),
            Some((c, Direction::Descending)) if c == column => None,
            _ => Some((column, Direction::Ascending)),
        };
        Self::derive(ready, ctx)
    }

    /// A sorted or filtered view finished draining.
    pub fn derived(
        &mut self,
        base: RunId,
        view: RunId,
        result: anyhow::Result<StoredQuery>,
        ctx: &Context,
    ) -> Option<Flash> {
        let State::Ready(ready) = &mut self.state else {
            return None;
        };
        if ready.base.run != base || ready.pending != Some(view) {
            return None;
        }
        ready.pending = None;
        let dir = ResultDir::new(ready.base.dir.path().join(format!("view-{view}")));
        let data = match result {
            Ok(mut stored) => match stored.truncation {
                Some(Truncation::Failed(e)) => return Some(Flash::Error(error_chain(&e))),
                _ => stored.result_sets.pop().and_then(|set| set.data),
            },
            Err(e) => return Some(Flash::Error(format!("{e:#}"))),
        };
        let data = data?;
        let page_rows = ctx.options().page_rows.get();
        match Grid::new(view, Arc::new(data), Some(dir), page_rows) {
            Ok(grid) => {
                let previous = std::mem::replace(&mut ready.grid, grid);
                ready.grid.follow(&previous);
                None
            }
            Err(e) => Some(Flash::Error(format!("{e:#}"))),
        }
    }

    /// Export the shown rows, in their shown order, to `path`.
    pub fn save(&self, path: &Path, ctx: &Context) -> Option<Flash> {
        let State::Ready(ready) = &self.state else {
            return Some(Flash::Error("no results to save".into()));
        };
        let target = path.to_string_lossy().replace('\'', "''");
        let sql = format!(
            "COPY (SELECT {} FROM {} ORDER BY {ROW_COLUMN}) TO '{target}'",
            ready.base.select_list(),
            crate::task::page::TABLE,
        );
        let messages = ctx.shared.messages.clone();
        derive::spawn_save(
            ready.grid.context().clone(),
            sql,
            path.to_path_buf(),
            messages,
        );
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
            return match self.cycle_sort(ctx) {
                Ok(()) => KeyOutcome::Consumed,
                Err(e) => KeyOutcome::Action(Action::Report(format!("{e:#}"))),
            };
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
                let sort = ready.spec.sort.as_ref().map(|(c, d)| (c.as_str(), *d));
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
