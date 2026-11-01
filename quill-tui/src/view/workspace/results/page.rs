//! The current page of rows plus one on each side, fetched in the background.
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use crate::task::page::{Page, PageRow};

pub struct PageCache {
    page_rows: u64,
    total: u64,
    pages: HashMap<u64, Page>,
    /// Requested and still wanted; a response for anything else is dropped.
    pending: HashSet<u64>,
}

impl PageCache {
    pub fn new(page_rows: u64, total: u64) -> Self {
        Self {
            page_rows: page_rows.max(1),
            total,
            pages: HashMap::new(),
            pending: HashSet::new(),
        }
    }

    pub fn row(&self, row: u64) -> Option<&PageRow> {
        let page = self.pages.get(&(row / self.page_rows))?;
        let found = page.rows.get(usize::try_from(row % self.page_rows).ok()?)?;
        (found.row == row).then_some(found)
    }

    /// Keep the pages covering `visible` plus one either side; return the
    /// ones to fetch now as `(index, rows)`.
    pub fn request(&mut self, visible: Range<u64>) -> Vec<(u64, Range<u64>)> {
        if self.total == 0 {
            return Vec::new();
        }
        let last_page = (self.total - 1) / self.page_rows;
        let first = (visible.start / self.page_rows).saturating_sub(1);
        let last = (visible.end.saturating_sub(1) / self.page_rows + 1).min(last_page);
        let wanted = first..=last;
        self.pages.retain(|index, _| wanted.contains(index));
        self.pending.retain(|index| wanted.contains(index));
        wanted
            .filter(|index| !self.pages.contains_key(index) && self.pending.insert(*index))
            .map(|index| {
                let start = index * self.page_rows;
                (index, start..(start + self.page_rows).min(self.total))
            })
            .collect()
    }

    /// Store a fetched page if it is still wanted.
    pub fn insert(&mut self, index: u64, page: Page) -> bool {
        let wanted = self.pending.remove(&index);
        if wanted {
            self.pages.insert(index, page);
        }
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rows: Range<u64>) -> Page {
        Page {
            rows: rows
                .map(|row| PageRow {
                    row,
                    cells: vec![crate::task::page::Cell::Text(row.to_string())],
                })
                .collect(),
        }
    }

    #[test]
    fn requests_neighbours_once_and_drops_stale_pages() {
        let mut cache = PageCache::new(10, 45);
        assert_eq!(cache.request(0..5), [(0, 0..10), (1, 10..20)]);
        assert!(cache.request(0..5).is_empty());
        assert!(cache.insert(0, page(0..10)));
        assert_eq!(cache.row(3).map(|r| r.cells[0].as_str()), Some("3"));

        // Jump to the end: pages 0 and 1 are no longer wanted.
        assert_eq!(cache.request(40..45), [(3, 30..40), (4, 40..45)]);
        assert!(cache.row(3).is_none());
        assert!(!cache.insert(1, page(10..20)));
    }
}
