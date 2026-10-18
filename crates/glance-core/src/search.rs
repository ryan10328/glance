//! Search engine: finds matching Entries while every Entry stays visible.
//!
//! A match is an Entry, not an occurrence: one Entry counts once however many
//! times the text appears in it. [`Search::occurrences`] still reports every
//! occurrence so the UI can color them. Search runs over the rows the user can
//! see (the [`FilteredView`], or all Entries when no Filter is set).

use std::io;
use std::ops::Range;

use regex::bytes::Regex;

use crate::filter::{FilteredView, MAX_CONTEXT, MAX_ENTRY_BYTES, FilterError, TextPattern, build_regex, read_entry};
use crate::index::EntryIndex;
use crate::reader::BlockReader;

/// The rows a Search runs over. Row numbers are what the user scrolls through.
pub trait Rows {
    fn row_count(&self) -> usize;
    /// The Entry shown at `row`.
    fn entry_at(&self, row: usize) -> Option<usize>;
}

impl Rows for FilteredView {
    fn row_count(&self) -> usize {
        self.len()
    }

    fn entry_at(&self, row: usize) -> Option<usize> {
        self.row(row).map(|r| r.entry)
    }
}

/// Every Entry of an index, in order (no Filter).
pub struct AllEntries<'a>(pub &'a EntryIndex);

impl Rows for AllEntries<'_> {
    fn row_count(&self) -> usize {
        self.0.len()
    }

    fn entry_at(&self, row: usize) -> Option<usize> {
        (row < self.0.len()).then_some(row)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: TextPattern,
    /// Off by default.
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// A match position: the row and its 1-based place among all matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub row: usize,
    pub ordinal: usize,
}

pub struct Search {
    /// `None` for an empty query, which matches nothing.
    regex: Option<Regex>,
    /// Matching row numbers, ascending.
    matches: Vec<u32>,
    scanned: usize,
}

impl Search {
    pub fn new(query: &SearchQuery) -> Result<Self, FilterError> {
        let source = (!query.pattern.is_empty()).then(|| query.pattern.to_regex_source());
        let regex = source.map(|s| build_regex(&s, query.case_sensitive)).transpose()?;
        Ok(Self { regex, matches: Vec::new(), scanned: 0 })
    }

    /// Number of matching Entries found so far.
    pub fn count(&self) -> usize {
        self.matches.len()
    }

    /// Rows examined so far.
    pub fn scanned_rows(&self) -> usize {
        self.scanned
    }

    pub fn matches(&self) -> impl Iterator<Item = usize> + '_ {
        self.matches.iter().map(|&r| r as usize)
    }

    /// Examines up to `max_rows` more rows and returns how many were new since
    /// the last call; call until it returns 0. The final rows are examined
    /// again each call because the Entries behind them may have grown or the
    /// Filter may have rewritten them.
    pub fn update(
        &mut self,
        rows: &dyn Rows,
        index: &EntryIndex,
        reader: &BlockReader,
        max_rows: usize,
    ) -> io::Result<usize> {
        let seen = self.scanned;
        let Some(regex) = &self.regex else {
            self.scanned = rows.row_count();
            return Ok(self.scanned.saturating_sub(seen));
        };
        // Rows near the end can still change; look at them again.
        let restart = self.scanned.saturating_sub(MAX_CONTEXT + 1).min(rows.row_count());
        let keep = self.matches.partition_point(|&m| (m as usize) < restart);
        self.matches.truncate(keep);
        // The budget counts new rows only; the re-checked tail is extra.
        let end = rows.row_count().min(seen.saturating_add(max_rows));
        let mut buf = Vec::new();
        for row in restart..end {
            let Some(entry) = rows.entry_at(row) else { break };
            read_entry(index, reader, entry, &mut buf)?;
            if regex.is_match(&buf) {
                self.matches.push(row as u32);
            }
        }
        self.scanned = end;
        Ok(end.saturating_sub(seen))
    }

    /// The next match after `from` in `direction`. With `from == None` it starts
    /// at the first (or last) match. At the end it wraps around if `wrap`.
    pub fn step(&self, from: Option<usize>, direction: Direction, wrap: bool) -> Option<Hit> {
        if self.matches.is_empty() {
            return None;
        }
        let index = match (direction, from) {
            (Direction::Forward, None) => 0,
            (Direction::Backward, None) => self.matches.len() - 1,
            (Direction::Forward, Some(row)) => {
                let i = self.matches.partition_point(|&m| (m as usize) <= row);
                if i < self.matches.len() {
                    i
                } else if wrap {
                    0
                } else {
                    return None;
                }
            }
            (Direction::Backward, Some(row)) => {
                let i = self.matches.partition_point(|&m| (m as usize) < row);
                if i > 0 {
                    i - 1
                } else if wrap {
                    self.matches.len() - 1
                } else {
                    return None;
                }
            }
        };
        Some(Hit { row: self.matches[index] as usize, ordinal: index + 1 })
    }

    /// The 1-based place of `row` among the matches, if it is one.
    pub fn ordinal(&self, row: usize) -> Option<usize> {
        self.matches.binary_search(&(row as u32)).ok().map(|i| i + 1)
    }

    /// Every occurrence of the query inside one Entry's bytes, for coloring.
    pub fn occurrences(&self, entry: &[u8]) -> Vec<Range<usize>> {
        let Some(regex) = &self.regex else { return Vec::new() };
        let entry = &entry[..entry.len().min(MAX_ENTRY_BYTES)];
        regex.find_iter(entry).filter(|m| !m.is_empty()).map(|m| m.range()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{CompiledFilter, Condition, Filter, FilterContext, TextCondition};
    use jiff::tz::TimeZone;
    use std::io::Write;

    struct Fixture {
        _file: tempfile::NamedTempFile,
        reader: BlockReader,
        index: EntryIndex,
        ctx: FilterContext,
    }

    fn fixture(data: &str) -> Fixture {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data.as_bytes()).unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let (format, mut index) = EntryIndex::open(&reader, None).unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        index.commit_tail();
        let ctx = FilterContext { format, rule: index.rule(), zone: TimeZone::UTC };
        Fixture { _file: file, reader, index, ctx }
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery { pattern: TextPattern::Wildcard(text.to_owned()), case_sensitive: false }
    }

    fn search_all(fx: &Fixture, q: &SearchQuery) -> Search {
        let mut search = Search::new(q).unwrap();
        search.update(&AllEntries(&fx.index), &fx.index, &fx.reader, usize::MAX).unwrap();
        search
    }

    const LOG: &str = "\
2026-10-18 19:50:01 INFO  server started
2026-10-18 19:50:02 INFO  healthcheck ok
2026-10-18 19:50:03 ERROR request failed: timeout, retry after timeout
    at handler.rs:42
2026-10-18 19:50:04 INFO  healthcheck ok
2026-10-18 19:50:05 WARN  slow query: TIMEOUT soon
";

    #[test]
    fn an_entry_counts_once_however_often_it_matches() {
        let fx = fixture(LOG);
        let search = search_all(&fx, &query("timeout"));
        assert_eq!(search.matches().collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(search.count(), 2);
    }

    #[test]
    fn matches_include_continuation_lines() {
        let fx = fixture(LOG);
        assert_eq!(search_all(&fx, &query("handler.rs")).matches().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn case_sensitivity_and_regex_are_options() {
        let fx = fixture(LOG);
        let sensitive = SearchQuery { case_sensitive: true, ..query("timeout") };
        assert_eq!(search_all(&fx, &sensitive).matches().collect::<Vec<_>>(), vec![2]);
        let regex = SearchQuery { pattern: TextPattern::Regex(r"health\w+ ok".into()), case_sensitive: false };
        assert_eq!(search_all(&fx, &regex).matches().collect::<Vec<_>>(), vec![1, 3]);
        let bad = SearchQuery { pattern: TextPattern::Regex("(".into()), case_sensitive: false };
        assert!(Search::new(&bad).is_err());
    }

    #[test]
    fn empty_query_matches_nothing() {
        let fx = fixture(LOG);
        let search = search_all(&fx, &query(""));
        assert_eq!(search.count(), 0);
        assert!(search.step(None, Direction::Forward, true).is_none());
        assert!(search.occurrences(b"anything").is_empty());
    }

    #[test]
    fn occurrences_lists_every_match_in_an_entry() {
        let fx = fixture(LOG);
        let search = search_all(&fx, &query("timeout"));
        let entry = b"failed: timeout, retry after timeout";
        assert_eq!(search.occurrences(entry), vec![8..15, 29..36]);
    }

    #[test]
    fn search_runs_over_visible_rows_only() {
        let fx = fixture(LOG);
        // Hide the healthchecks: remaining Entries are 0, 2, 4.
        let hide = Condition::Text(TextCondition {
            pattern: TextPattern::Wildcard("healthcheck".into()),
            case_sensitive: true,
            exclude: true,
        });
        let compiled = CompiledFilter::new(&Filter { conditions: vec![hide], ..Filter::default() }).unwrap();
        let mut view = FilteredView::new();
        view.update(&compiled, &fx.index, &fx.reader, &fx.ctx, usize::MAX).unwrap();
        assert_eq!(view.len(), 3);

        let mut search = Search::new(&query("timeout")).unwrap();
        search.update(&view, &fx.index, &fx.reader, usize::MAX).unwrap();
        // Rows, not Entry numbers: Entries 2 and 4 are rows 1 and 2.
        assert_eq!(search.matches().collect::<Vec<_>>(), vec![1, 2]);

        // A match hidden by the Filter is not found.
        let mut hidden = Search::new(&query("healthcheck")).unwrap();
        hidden.update(&view, &fx.index, &fx.reader, usize::MAX).unwrap();
        assert_eq!(hidden.count(), 0);
    }

    #[test]
    fn stepping_forward_backward_and_wrapping() {
        let fx = fixture(LOG);
        let search = search_all(&fx, &query("healthcheck")); // rows 1 and 3
        let hit = |row, ordinal| Some(Hit { row, ordinal });
        assert_eq!(search.step(None, Direction::Forward, false), hit(1, 1));
        assert_eq!(search.step(Some(1), Direction::Forward, false), hit(3, 2));
        assert_eq!(search.step(Some(3), Direction::Forward, false), None);
        assert_eq!(search.step(Some(3), Direction::Forward, true), hit(1, 1));
        assert_eq!(search.step(None, Direction::Backward, false), hit(3, 2));
        assert_eq!(search.step(Some(3), Direction::Backward, false), hit(1, 1));
        assert_eq!(search.step(Some(1), Direction::Backward, false), None);
        assert_eq!(search.step(Some(1), Direction::Backward, true), hit(3, 2));
        // From a row between matches.
        assert_eq!(search.step(Some(2), Direction::Forward, false), hit(3, 2));
        assert_eq!(search.step(Some(2), Direction::Backward, false), hit(1, 1));
        assert_eq!(search.ordinal(3), Some(2));
        assert_eq!(search.ordinal(2), None);
    }

    #[test]
    fn incremental_updates_match_a_single_pass() {
        let fx = fixture(&LOG.repeat(40));
        let whole = search_all(&fx, &query("timeout"));
        let mut stepped = Search::new(&query("timeout")).unwrap();
        let rows = AllEntries(&fx.index);
        while stepped.update(&rows, &fx.index, &fx.reader, 7).unwrap() > 0 {}
        assert_eq!(stepped.matches().collect::<Vec<_>>(), whole.matches().collect::<Vec<_>>());
        assert_eq!(stepped.count(), 80);
    }

    #[test]
    fn growth_of_the_newest_entry_is_picked_up_without_duplicates() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 INFO b\n").unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let (_, mut index) = EntryIndex::open(&reader, None).unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        let mut search = Search::new(&query("boom")).unwrap();
        search.update(&AllEntries(&index), &index, &reader, usize::MAX).unwrap();
        assert_eq!(search.count(), 0);

        file.write_all(b"    boom\n2026-10-18 19:50:03 INFO c\n").unwrap();
        file.flush().unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        search.update(&AllEntries(&index), &index, &reader, usize::MAX).unwrap();
        search.update(&AllEntries(&index), &index, &reader, usize::MAX).unwrap();
        assert_eq!(search.matches().collect::<Vec<_>>(), vec![1]);
    }
}
