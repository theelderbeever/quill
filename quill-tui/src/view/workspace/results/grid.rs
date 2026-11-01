//! A paged, column-scrolled view of one stored result.
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use datafusion::prelude::SessionContext;
use quill_io::feather::{FeatherTable, StoredResult};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::Span,
    widgets::{Row, Table, TableState},
};

use super::{
    Direction,
    columns::{self, Widths},
    inspector::Inspector,
    page::PageCache,
};
use crate::{
    component::Context,
    task::{
        RunId,
        page::{self, Cell, Page, TABLE},
        run::ResultDir,
    },
    widget::theme,
};

/// Rows moved per mouse wheel notch.
const WHEEL_ROWS: u64 = 3;

pub enum GridOutcome {
    Consumed,
    Ignored,
    Inspect(Inspector),
}

/// Where a column was drawn, for mouse hit testing.
#[derive(Clone, Copy)]
struct DrawnColumn {
    index: usize,
    x: u16,
    width: u16,
}

pub struct Grid {
    run: RunId,
    ctx: SessionContext,
    columns: Vec<String>,
    rows: u64,
    cursor_row: u64,
    cursor_col: usize,
    top: u64,
    left: usize,
    /// Data rows visible at the last draw, for paging keys.
    height: u64,
    /// Widths used at the last draw, for keyboard resizing.
    drawn_widths: Vec<u16>,
    drawn: Vec<DrawnColumn>,
    /// First data row's screen line at the last draw.
    body_y: u16,
    /// Column whose right edge is being dragged, and its left edge.
    dragging: Option<(usize, u16)>,
    pages: PageCache,
    _dir: Option<ResultDir>,
}

impl Grid {
    /// `dir` is removed with the grid; `None` when another owner keeps it.
    pub fn new(
        run: RunId,
        result: Arc<StoredResult>,
        dir: Option<ResultDir>,
        page_rows: u64,
    ) -> anyhow::Result<Self> {
        let columns = result
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        let rows = result.rows();
        let ctx = SessionContext::new();
        ctx.register_table(TABLE, Arc::new(FeatherTable::new(result)?))?;
        Ok(Self {
            run,
            ctx,
            columns,
            rows,
            cursor_row: 0,
            cursor_col: 0,
            top: 0,
            left: 0,
            height: 1,
            drawn_widths: Vec::new(),
            drawn: Vec::new(),
            body_y: 0,
            dragging: None,
            pages: PageCache::new(page_rows, rows),
            _dir: dir,
        })
    }

    pub const fn run(&self) -> RunId {
        self.run
    }

    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Context with this result registered as `r`, including `__row`.
    pub const fn context(&self) -> &SessionContext {
        &self.ctx
    }

    /// The full value under the cursor, if its page is loaded.
    pub fn cell(&self) -> Option<&str> {
        let row = self.pages.row(self.cursor_row)?;
        row.cells.get(self.cursor_col).map(Cell::as_str)
    }

    pub fn column(&self) -> Option<&str> {
        self.columns.get(self.cursor_col).map(String::as_str)
    }

    /// Keep the cursor on the same column after a re-run or new view.
    pub fn follow(&mut self, previous: &Self) {
        self.cursor_col = previous
            .cursor_col
            .min(self.columns.len().saturating_sub(1));
        self.left = previous.left.min(self.cursor_col);
    }

    pub fn insert_page(&mut self, index: u64, page: Page) -> bool {
        self.pages.insert(index, page)
    }

    pub fn handle_key(&mut self, key: KeyEvent, widths: &mut Widths) -> GridOutcome {
        let last_row = self.rows.saturating_sub(1);
        let last_col = self.columns.len().saturating_sub(1);
        let half = (self.height / 2).max(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor_row = (self.cursor_row + 1).min(last_row)
            }
            KeyCode::Char('k') | KeyCode::Up => self.cursor_row = self.cursor_row.saturating_sub(1),
            KeyCode::Char('d') if ctrl => self.cursor_row = (self.cursor_row + half).min(last_row),
            KeyCode::Char('u') if ctrl => self.cursor_row = self.cursor_row.saturating_sub(half),
            KeyCode::PageDown => self.cursor_row = (self.cursor_row + self.height).min(last_row),
            KeyCode::PageUp => self.cursor_row = self.cursor_row.saturating_sub(self.height),
            KeyCode::Char('g') | KeyCode::Home => self.cursor_row = 0,
            KeyCode::Char('G') | KeyCode::End => self.cursor_row = last_row,
            KeyCode::Char('l') | KeyCode::Right => {
                self.cursor_col = (self.cursor_col + 1).min(last_col)
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1)
            }
            KeyCode::Char('0') => self.cursor_col = 0,
            KeyCode::Char('$') => self.cursor_col = last_col,
            KeyCode::Char(c @ ('-' | '+')) => {
                let current = self.drawn_widths.get(self.cursor_col).copied().unwrap_or(1);
                if let Some(name) = self.columns.get(self.cursor_col) {
                    widths.adjust(name, current, if c == '-' { -1 } else { 1 });
                }
            }
            KeyCode::Char('=') => {
                if let Some(name) = self.columns.get(self.cursor_col) {
                    widths.reset(name);
                }
            }
            KeyCode::Enter => {
                let cell = self.pages.row(self.cursor_row).and_then(|row| {
                    let value = row.cells.get(self.cursor_col)?;
                    Some(Inspector::new(
                        &self.columns[self.cursor_col],
                        row.row + 1,
                        value.as_str(),
                    ))
                });
                return cell.map_or(GridOutcome::Consumed, GridOutcome::Inspect);
            }
            _ => return GridOutcome::Ignored,
        }
        GridOutcome::Consumed
    }

    /// Click a cell to move there; drag the gap after a column to resize it;
    /// the wheel scrolls rows.
    pub fn handle_mouse(&mut self, event: MouseEvent, widths: &mut Widths) {
        let last_row = self.rows.saturating_sub(1);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(column) = self.drawn.iter().find(|c| event.column == c.x + c.width) {
                    self.dragging = Some((column.index, column.x));
                } else if let Some(column) = self
                    .drawn
                    .iter()
                    .find(|c| (c.x..c.x + c.width).contains(&event.column))
                {
                    self.cursor_col = column.index;
                    if let Some(offset) = event.row.checked_sub(self.body_y) {
                        self.cursor_row = (self.top + u64::from(offset)).min(last_row);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((index, x)) = self.dragging
                    && let Some(name) = self.columns.get(index)
                {
                    widths.set(name, event.column.saturating_sub(x));
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = None,
            MouseEventKind::ScrollDown => {
                self.cursor_row = (self.cursor_row + WHEEL_ROWS).min(last_row)
            }
            MouseEventKind::ScrollUp => {
                self.cursor_row = self.cursor_row.saturating_sub(WHEEL_ROWS)
            }
            _ => {}
        }
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &Context,
        widths: &Widths,
        sort: Option<(&str, Direction)>,
    ) {
        if area.height < 2 || area.width == 0 {
            return;
        }
        self.height = u64::from(area.height - 1);
        self.body_y = area.y + 1;
        if self.cursor_row < self.top {
            self.top = self.cursor_row;
        } else if self.cursor_row >= self.top + self.height {
            self.top = self.cursor_row + 1 - self.height;
        }
        let visible = self.top..(self.top + self.height).min(self.rows);
        for (index, rows) in self.pages.request(visible.clone()) {
            page::spawn(
                self.ctx.clone(),
                self.run,
                index,
                rows,
                ctx.shared.messages.clone(),
            );
        }

        let header_name = |name: &str| match sort {
            Some((column, direction)) if column == name => format!("{name} {}", direction.arrow()),
            _ => name.to_owned(),
        };
        let cap = usize::from(ctx.options().max_column_width.max(1));
        self.drawn_widths = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let longest = visible
                    .clone()
                    .filter_map(|r| {
                        self.pages
                            .row(r)?
                            .cells
                            .get(i)
                            .map(|c| columns::display_width(c.display()))
                    })
                    .chain([columns::display_width(&header_name(name))])
                    .max()
                    .unwrap_or(1);
                let auto = u16::try_from(longest.clamp(1, cap)).unwrap_or(u16::MAX);
                widths.get(name, auto)
            })
            .collect();

        let gutter = u16::try_from(self.rows.max(1).to_string().len()).unwrap_or(u16::MAX);
        let available = area.width.saturating_sub(gutter + 1);
        let shown = columns::window(&self.drawn_widths, self.left, self.cursor_col, available);
        self.left = shown.start;
        // The cursor only lands on whole columns; the glimpse is just a hint.
        let glimpse = columns::partial(&self.drawn_widths, &shown, available);
        let drawn: Vec<(usize, u16)> = shown
            .clone()
            .map(|i| (i, self.drawn_widths[i]))
            .chain(glimpse.map(|w| (shown.end, w)))
            .collect();
        let mut x = area.x + gutter + 1;
        self.drawn = drawn
            .iter()
            .map(|&(index, width)| {
                let column = DrawnColumn { index, x, width };
                x = x.saturating_add(width + 1);
                column
            })
            .collect();

        let header = std::iter::once(Span::raw("")).chain(drawn.iter().map(|&(i, width)| {
            let style = if i == self.cursor_col {
                theme::KEY
            } else {
                theme::TITLE
            };
            Span::styled(columns::fit(&header_name(&self.columns[i]), width), style)
        }));
        let body = visible.clone().map(|r| {
            let number = Span::styled((r + 1).to_string(), theme::GUTTER);
            let cells: Vec<Span> = match self.pages.row(r) {
                Some(row) => drawn
                    .iter()
                    .map(|&(i, width)| match row.cells.get(i) {
                        Some(cell) => {
                            let text = columns::fit(cell.display(), width);
                            if matches!(cell, Cell::Binary(_)) {
                                Span::styled(text, theme::DIM)
                            } else {
                                Span::raw(text)
                            }
                        }
                        None => Span::raw(""),
                    })
                    .collect(),
                None => vec![Span::styled("…", theme::DIM)],
            };
            Row::new(std::iter::once(number).chain(cells))
        });
        let constraints = std::iter::once(Constraint::Length(gutter))
            .chain(drawn.iter().map(|&(_, width)| Constraint::Length(width)));
        let cell_style = if ctx.focused {
            theme::CELL_CURSOR
        } else {
            theme::ROW_CURSOR
        };
        let table = Table::new(body, constraints)
            .header(Row::new(header))
            .row_highlight_style(theme::ROW_CURSOR)
            .cell_highlight_style(cell_style);
        let mut state = TableState::new()
            .with_selected(usize::try_from(self.cursor_row - self.top).ok())
            .with_selected_column(Some(self.cursor_col - shown.start + 1));
        frame.render_stateful_widget(table, area, &mut state);
    }
}
