//! Entry selection: click, Shift-click range and Cmd/Ctrl-click toggle.
//!
//! Stored as sorted, disjoint ranges of Entry numbers (not view rows), so the
//! selection survives a change of Filter: Entries that become hidden are
//! dropped with [`Selection::retain`], the rest stay selected.

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

    /// The Entry a Shift-click range grows from.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Shift-click: replaces the selection with `entries` (ascending, the
    /// Entries between the anchor and the clicked one as the view shows them)
    /// and keeps the anchor.
    pub fn replace_with(&mut self, entries: impl IntoIterator<Item = usize>) {
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for entry in entries {
            match ranges.last_mut() {
                Some(last) if last.end == entry => last.end += 1,
                _ => ranges.push(entry..entry + 1),
            }
        }
        self.ranges = ranges;
    }

    /// Keeps only the selected Entries for which `keep` is true.
    pub fn retain(&mut self, keep: impl Fn(usize) -> bool) {
        let kept: Vec<usize> = self.rows().filter(|&e| keep(e)).collect();
        let anchor = self.anchor;
        self.replace_with(kept);
        self.anchor = anchor;
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
    fn replace_with_builds_ranges_and_keeps_the_anchor() {
        let mut s = Selection::default();
        s.click(10);
        s.replace_with([10, 11, 12, 20, 21]);
        assert_eq!(rows(&s), vec![10, 11, 12, 20, 21]);
        assert_eq!(s.ranges, vec![10..13, 20..22]);
        assert_eq!(s.anchor(), Some(10));
    }

    #[test]
    fn retain_drops_hidden_entries() {
        let mut s = Selection::default();
        s.click(1);
        s.replace_with(1..8);
        s.retain(|e| e % 2 == 0);
        assert_eq!(rows(&s), vec![2, 4, 6]);
        assert_eq!(s.anchor(), Some(1));
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
        s.replace_with(0..=50_000_000);
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
        assert_eq!(s.anchor(), None);
        assert!(s.is_empty());
    }
}
