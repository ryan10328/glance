//! Row selection: click, Shift-click range and Cmd/Ctrl-click toggle.
//!
//! Stored as sorted, disjoint ranges so selecting millions of rows costs
//! nothing. Rows are row numbers of the view, not Entry numbers.

use std::ops::Range;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    ranges: Vec<Range<usize>>,
    anchor: Option<usize>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Number of selected rows.
    pub fn len(&self) -> usize {
        self.ranges.iter().map(|r| r.len()).sum()
    }

    pub fn contains(&self, row: usize) -> bool {
        let i = self.ranges.partition_point(|r| r.end <= row);
        self.ranges.get(i).is_some_and(|r| r.start <= row)
    }

    /// Selected rows in ascending order.
    pub fn rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges.iter().flat_map(|r| r.clone())
    }

    pub fn clear(&mut self) {
        self.ranges.clear();
        self.anchor = None;
    }

    /// Plain click: select only `row`.
    pub fn click(&mut self, row: usize) {
        self.ranges = std::iter::once(row..row + 1).collect();
        self.anchor = Some(row);
    }

    /// Shift-click: select from the anchor to `row`, replacing the selection.
    pub fn extend_to(&mut self, row: usize) {
        match self.anchor {
            Some(anchor) => self.ranges = std::iter::once(anchor.min(row)..anchor.max(row) + 1).collect(),
            None => self.click(row),
        }
    }

    /// Cmd/Ctrl-click: add `row`, or remove it when already selected.
    pub fn toggle(&mut self, row: usize) {
        self.anchor = Some(row);
        let i = self.ranges.partition_point(|r| r.end <= row);
        if let Some(range) = self.ranges.get(i).filter(|r| r.start <= row).cloned() {
            let mut pieces = Vec::new();
            if range.start < row {
                pieces.push(range.start..row);
            }
            if row + 1 < range.end {
                pieces.push(row + 1..range.end);
            }
            self.ranges.splice(i..=i, pieces);
        } else {
            self.ranges.insert(i, row..row + 1);
            self.merge_around(i);
        }
    }

    fn merge_around(&mut self, i: usize) {
        if i + 1 < self.ranges.len() && self.ranges[i].end == self.ranges[i + 1].start {
            self.ranges[i].end = self.ranges[i + 1].end;
            self.ranges.remove(i + 1);
        }
        if i > 0 && self.ranges[i - 1].end == self.ranges[i].start {
            self.ranges[i - 1].end = self.ranges[i].end;
            self.ranges.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &Selection) -> Vec<usize> {
        s.rows().collect()
    }

    #[test]
    fn click_selects_a_single_row() {
        let mut s = Selection::default();
        s.click(5);
        s.click(7);
        assert_eq!(rows(&s), vec![7]);
        assert!(s.contains(7) && !s.contains(5));
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor_in_either_direction() {
        let mut s = Selection::default();
        s.click(10);
        s.extend_to(13);
        assert_eq!(rows(&s), vec![10, 11, 12, 13]);
        s.extend_to(8);
        assert_eq!(rows(&s), vec![8, 9, 10]);
        let mut fresh = Selection::default();
        fresh.extend_to(4);
        assert_eq!(rows(&fresh), vec![4]);
    }

    #[test]
    fn toggle_adds_removes_splits_and_merges() {
        let mut s = Selection::default();
        s.click(1);
        s.toggle(3);
        assert_eq!(rows(&s), vec![1, 3]);
        s.toggle(2); // bridges the two
        assert_eq!(s.ranges, vec![1..4]);
        s.toggle(2); // splits again
        assert_eq!(rows(&s), vec![1, 3]);
        s.toggle(1);
        s.toggle(3);
        assert!(s.is_empty());
    }

    #[test]
    fn huge_ranges_are_cheap() {
        let mut s = Selection::default();
        s.click(0);
        s.extend_to(50_000_000);
        assert_eq!(s.len(), 50_000_001);
        assert!(s.contains(25_000_000) && !s.contains(50_000_001));
        s.toggle(10);
        assert_eq!(s.len(), 50_000_000);
    }

    #[test]
    fn clear_forgets_the_anchor() {
        let mut s = Selection::default();
        s.click(3);
        s.clear();
        s.extend_to(9);
        assert_eq!(rows(&s), vec![9]);
    }
}
