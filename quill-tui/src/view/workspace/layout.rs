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
    catalog: Option<Rect>,
    editor: Rect,
    results: Rect,
    stats: Option<Rect>,
}

impl WorkspaceLayout {
    pub fn split(area: Rect, visibility: Visibility) -> Self {
        let catalog = visibility.shows(Sidebar::Catalog, area.width);
        let stats = visibility.shows(Sidebar::Stats, area.width);
        let width = |shown, width| Constraint::Length(if shown { width } else { 0 });
        let [catalog_area, center, stats_area] = Layout::horizontal([
            width(catalog, CATALOG_WIDTH),
            Constraint::Min(0),
            width(stats, STATS_WIDTH),
        ])
        .areas(area);
        let [editor, results] =
            Layout::vertical([Constraint::Percentage(EDITOR_PERCENT), Constraint::Min(0)])
                .areas(center);
        Self {
            catalog: catalog.then_some(catalog_area),
            editor,
            results,
            stats: stats.then_some(stats_area),
        }
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
        WorkspaceLayout::split(Rect::new(0, 0, width, 40), visibility)
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
}
