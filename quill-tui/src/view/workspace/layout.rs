//! Catalog | editor over results | statistics, shedding sidebars as the
//! terminal narrows.
use ratatui::layout::{Constraint, Layout, Position, Rect};

use super::Panel;

/// Below this width the statistics column hides unless toggled on.
pub const HIDE_STATS_BELOW: u16 = 120;
/// Below this width the catalog tree hides unless toggled on.
pub const HIDE_CATALOG_BELOW: u16 = 80;
const CATALOG_WIDTH: u16 = 30;
const STATS_WIDTH: u16 = 34;
const EDITOR_PERCENT: u16 = 40;
/// Narrowest a sidebar may be resized to.
const MIN_SIDEBAR: u16 = 10;
/// Width always left to the editor and results.
const MIN_CENTER: u16 = 20;
/// The editor's share of the centre column stays within this range.
const EDITOR_PERCENT_RANGE: (u16, u16) = (10, 90);
/// Cells or percent moved per resize key press.
const WIDTH_STEP: u16 = 2;
const PERCENT_STEP: u16 = 5;

/// Panel sizes: sidebar widths in cells and the editor's share of the
/// centre column, in percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    catalog: u16,
    stats: u16,
    editor_percent: u16,
}

impl Sizes {
    pub const DEFAULT: Self = Self {
        catalog: CATALOG_WIDTH,
        stats: STATS_WIDTH,
        editor_percent: EDITOR_PERCENT,
    };

    fn set_editor(&mut self, percent: u16) {
        let (min, max) = EDITOR_PERCENT_RANGE;
        self.editor_percent = percent.clamp(min, max);
    }

    /// Grow (`wider`/`taller` positive) or shrink `panel` by one step.
    /// Sidebars only change width; the editor and results only height.
    pub fn resize(&mut self, panel: Panel, wider: i16, taller: i16) {
        let step = |value: u16, steps: i16, by: u16| value.saturating_add_signed(steps * by as i16);
        match panel {
            Panel::Catalog => self.catalog = step(self.catalog, wider, WIDTH_STEP).max(MIN_SIDEBAR),
            Panel::Stats => self.stats = step(self.stats, wider, WIDTH_STEP).max(MIN_SIDEBAR),
            Panel::Editor => self.set_editor(step(self.editor_percent, taller, PERCENT_STEP)),
            Panel::Results => self.set_editor(step(self.editor_percent, -taller, PERCENT_STEP)),
        }
    }

    /// Move `border` to `to`, as when dragged with the mouse.
    pub fn drag(&mut self, border: Border, to: Position, layout: &WorkspaceLayout) {
        let area = layout.area;
        match border {
            Border::Catalog => self.catalog = (to.x.saturating_sub(area.x) + 1).max(MIN_SIDEBAR),
            Border::Stats => self.stats = area.right().saturating_sub(to.x).max(MIN_SIDEBAR),
            Border::Editor => {
                let height = layout.editor.height + layout.results.height;
                let rows = to.y.saturating_sub(layout.editor.y) + 1;
                self.set_editor(rows * 100 / height.max(1));
            }
        }
    }
}

/// A border between panels that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    Catalog,
    Stats,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sidebar {
    Catalog,
    Stats,
}

impl Sidebar {
    const fn hide_below(self) -> u16 {
        match self {
            Self::Catalog => HIDE_CATALOG_BELOW,
            Self::Stats => HIDE_STATS_BELOW,
        }
    }
}

/// Explicit show or hide per sidebar; `None` follows terminal width.
#[derive(Debug, Clone, Copy)]
pub struct Visibility {
    catalog: Option<bool>,
    stats: Option<bool>,
}

impl Visibility {
    pub const AUTO: Self = Self {
        catalog: None,
        stats: None,
    };

    const fn slot(&mut self, sidebar: Sidebar) -> &mut Option<bool> {
        match sidebar {
            Sidebar::Catalog => &mut self.catalog,
            Sidebar::Stats => &mut self.stats,
        }
    }

    pub const fn shows(self, sidebar: Sidebar, width: u16) -> bool {
        let explicit = match sidebar {
            Sidebar::Catalog => self.catalog,
            Sidebar::Stats => self.stats,
        };
        match explicit {
            Some(shown) => shown,
            None => width >= sidebar.hide_below(),
        }
    }

    /// Flip what is currently shown at `width`.
    pub fn toggle(&mut self, sidebar: Sidebar, width: u16) {
        let shown = self.shows(sidebar, width);
        *self.slot(sidebar) = Some(!shown);
    }
}

pub struct WorkspaceLayout {
    area: Rect,
    catalog: Option<Rect>,
    editor: Rect,
    results: Rect,
    stats: Option<Rect>,
}

impl WorkspaceLayout {
    pub fn split(area: Rect, visibility: Visibility, sizes: Sizes) -> Self {
        let catalog = visibility.shows(Sidebar::Catalog, area.width);
        let stats = visibility.shows(Sidebar::Stats, area.width);
        // Sidebars give way so the centre keeps its minimum width.
        let room = area.width.saturating_sub(MIN_CENTER);
        let catalog_width = if catalog { sizes.catalog.min(room) } else { 0 };
        let stats_width = if stats {
            sizes.stats.min(room - catalog_width)
        } else {
            0
        };
        let [catalog_area, center, stats_area] = Layout::horizontal([
            Constraint::Length(catalog_width),
            Constraint::Min(0),
            Constraint::Length(stats_width),
        ])
        .areas(area);
        let [editor, results] = Layout::vertical([
            Constraint::Percentage(sizes.editor_percent),
            Constraint::Min(0),
        ])
        .areas(center);
        Self {
            area,
            catalog: catalog.then_some(catalog_area),
            editor,
            results,
            stats: stats.then_some(stats_area),
        }
    }

    /// The draggable border under `position`: the edge cells on either
    /// side of it.
    pub fn border_at(&self, position: Position) -> Option<Border> {
        let Position { x, y } = position;
        if !self.area.contains(position) {
            return None;
        }
        if let Some(catalog) = self.catalog
            && (x == catalog.right().saturating_sub(1) || x == catalog.right())
        {
            return Some(Border::Catalog);
        }
        if let Some(stats) = self.stats
            && (x == stats.x || x + 1 == stats.x)
        {
            return Some(Border::Stats);
        }
        let in_center = (self.editor.x..self.editor.right()).contains(&x);
        (in_center && (y + 1 == self.editor.bottom() || y == self.results.y))
            .then_some(Border::Editor)
    }

    pub fn panel_at(&self, position: Position) -> Option<Panel> {
        Panel::ORDER
            .into_iter()
            .find(|panel| self.get(*panel).is_some_and(|area| area.contains(position)))
    }

    pub const fn get(&self, panel: Panel) -> Option<Rect> {
        match panel {
            Panel::Catalog => self.catalog,
            Panel::Editor => Some(self.editor),
            Panel::Results => Some(self.results),
            Panel::Stats => self.stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(width: u16, visibility: Visibility) -> WorkspaceLayout {
        WorkspaceLayout::split(Rect::new(0, 0, width, 40), visibility, Sizes::DEFAULT)
    }

    #[test]
    fn sheds_stats_then_catalog_as_width_shrinks() {
        let wide = split(HIDE_STATS_BELOW, Visibility::AUTO);
        assert_eq!(wide.catalog.map(|r| r.width), Some(CATALOG_WIDTH));
        assert_eq!(wide.stats.map(|r| r.width), Some(STATS_WIDTH));
        assert_eq!(
            wide.editor.width,
            HIDE_STATS_BELOW - CATALOG_WIDTH - STATS_WIDTH
        );
        assert_eq!(wide.editor.width, wide.results.width);

        let medium = split(HIDE_STATS_BELOW - 1, Visibility::AUTO);
        assert!(medium.catalog.is_some());
        assert!(medium.stats.is_none());

        let narrow = split(HIDE_CATALOG_BELOW - 1, Visibility::AUTO);
        assert!(narrow.catalog.is_none());
        assert_eq!(narrow.editor.width, HIDE_CATALOG_BELOW - 1);
    }

    #[test]
    fn toggles_override_width() {
        let width = HIDE_CATALOG_BELOW - 1;
        let mut visibility = Visibility::AUTO;
        visibility.toggle(Sidebar::Stats, width);
        assert!(split(width, visibility).stats.is_some());

        let mut visibility = Visibility::AUTO;
        visibility.toggle(Sidebar::Catalog, u16::MAX);
        assert!(split(u16::MAX, visibility).catalog.is_none());
    }

    #[test]
    fn resizing_steps_and_clamps() {
        let mut sizes = Sizes::DEFAULT;
        sizes.resize(Panel::Catalog, 1, 0);
        assert_eq!(sizes.catalog, CATALOG_WIDTH + WIDTH_STEP);
        for _ in 0..50 {
            sizes.resize(Panel::Stats, -1, 0);
            sizes.resize(Panel::Results, 0, 1);
        }
        assert_eq!(sizes.stats, MIN_SIDEBAR);
        assert_eq!(sizes.editor_percent, EDITOR_PERCENT_RANGE.0);
        // Height keys do nothing to sidebars.
        sizes.resize(Panel::Catalog, 0, 1);
        assert_eq!(sizes.catalog, CATALOG_WIDTH + WIDTH_STEP);
    }

    #[test]
    fn sidebars_never_squeeze_the_centre_below_its_minimum() {
        let mut sizes = Sizes::DEFAULT;
        for _ in 0..100 {
            sizes.resize(Panel::Catalog, 1, 0);
        }
        let layout = WorkspaceLayout::split(Rect::new(0, 0, 130, 40), Visibility::AUTO, sizes);
        assert!(layout.editor.width >= MIN_CENTER, "{:?}", layout.editor);
    }

    #[test]
    fn dragging_borders_resizes() {
        let area = Rect::new(0, 0, 160, 40);
        let layout = WorkspaceLayout::split(area, Visibility::AUTO, Sizes::DEFAULT);
        let edge = Position::new(CATALOG_WIDTH - 1, 5);
        assert_eq!(layout.border_at(edge), Some(Border::Catalog));
        assert_eq!(
            layout.border_at(Position::new(160 - STATS_WIDTH, 5)),
            Some(Border::Stats)
        );
        assert_eq!(
            layout.border_at(Position::new(80, layout.editor.bottom() - 1)),
            Some(Border::Editor)
        );
        assert_eq!(layout.border_at(Position::new(80, 1)), None);

        let mut sizes = Sizes::DEFAULT;
        sizes.drag(Border::Catalog, Position::new(39, 5), &layout);
        assert_eq!(sizes.catalog, 40);
        sizes.drag(Border::Stats, Position::new(140, 5), &layout);
        assert_eq!(sizes.stats, 20);
        sizes.drag(Border::Editor, Position::new(80, 19), &layout);
        assert_eq!(sizes.editor_percent, 50);
    }
}
