//! Column widths and which columns fit on screen.
use std::{collections::HashMap, ops::Range};

use unicode_width::UnicodeWidthChar;

const ELLIPSIS: char = '…';

/// Cells are one line: line breaks and tabs show as spaces.
fn cell_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

fn char_width(c: char) -> usize {
    cell_char(c).width().unwrap_or(0)
}

pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` cut to `width` cells, ending in `…` when cut.
pub fn fit(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if display_width(text) <= width {
        return text.chars().map(cell_char).collect();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w + 1 > width {
            break;
        }
        out.push(cell_char(c));
        used += w;
    }
    if width > 0 {
        out.push(ELLIPSIS);
    }
    out
}

/// Widths set by hand, by column name, kept across pages and re-runs.
#[derive(Default)]
pub struct Widths {
    manual: HashMap<String, u16>,
}

impl Widths {
    pub fn get(&self, column: &str, auto: u16) -> u16 {
        self.manual.get(column).copied().unwrap_or(auto)
    }

    /// Shrink or widen from `current`, never below one cell.
    pub fn adjust(&mut self, column: &str, current: u16, delta: i16) {
        let width = current.saturating_add_signed(delta).max(1);
        self.manual.insert(column.to_owned(), width);
    }

    pub fn set(&mut self, column: &str, width: u16) {
        self.manual.insert(column.to_owned(), width.max(1));
    }

    pub fn reset(&mut self, column: &str) {
        self.manual.remove(column);
    }
}

/// Whole columns from `left` that fit in `available` cells with one-cell
/// gaps, moving `left` so `cursor` is among them. Always includes at least
/// the cursor column.
pub fn window(widths: &[u16], left: usize, cursor: usize, available: u16) -> Range<usize> {
    let fits_until = |start: usize| {
        let mut used = 0u32;
        let mut end = start;
        for (i, width) in widths.iter().enumerate().skip(start) {
            let needed = u32::from(*width) + u32::from(i > start);
            if used + needed > u32::from(available) && i > start {
                break;
            }
            used += needed;
            end = i + 1;
        }
        end
    };
    let mut left = left.min(cursor);
    loop {
        let end = fits_until(left);
        if cursor < end || left >= cursor {
            return left..end.max(left + 1).min(widths.len().max(left + 1));
        }
        left += 1;
    }
}

/// Cells left after the `shown` columns for a cut-off glimpse of the next
/// one, signalling more columns to the right. `None` when nothing follows or
/// no room is left beyond the gap.
pub fn partial(widths: &[u16], shown: &Range<usize>, available: u16) -> Option<u16> {
    if shown.end >= widths.len() {
        return None;
    }
    let used: u32 = widths[shown.clone()]
        .iter()
        .map(|w| u32::from(*w))
        .sum::<u32>()
        + u32::try_from(shown.len()).unwrap_or(u32::MAX);
    let left = u32::from(available).checked_sub(used)?;
    u16::try_from(left).ok().filter(|w| *w > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_text_with_an_ellipsis() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("a\nb", 5), "a b");
        assert_eq!(fit("日本語", 4), "日…");
    }

    #[test]
    fn window_scrolls_whole_columns_to_the_cursor() {
        let widths = [10, 10, 10, 10];
        assert_eq!(window(&widths, 0, 0, 21), 0..2);
        assert_eq!(window(&widths, 0, 2, 21), 1..3);
        assert_eq!(window(&widths, 3, 1, 21), 1..3);
        // A column wider than the screen still shows.
        assert_eq!(window(&[50, 5], 0, 0, 20), 0..1);
    }

    #[test]
    fn partial_column_fills_the_remainder() {
        let widths = [10, 10, 10];
        // Two columns and their gaps use 22 of 25 cells; the third gets 3.
        assert_eq!(partial(&widths, &(0..2), 25), Some(3));
        assert_eq!(partial(&widths, &(0..2), 22), None);
        assert_eq!(partial(&widths, &(0..3), 40), None);
    }

    #[test]
    fn manual_widths_override_and_reset() {
        let mut widths = Widths::default();
        assert_eq!(widths.get("a", 7), 7);
        widths.adjust("a", 7, -10);
        assert_eq!(widths.get("a", 7), 1);
        widths.adjust("a", 1, 3);
        assert_eq!(widths.get("a", 7), 4);
        widths.reset("a");
        assert_eq!(widths.get("a", 7), 7);
    }
}
