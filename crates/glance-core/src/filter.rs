//! Filter engine: hides every Entry that does not match, optionally keeping
//! surrounding Context.
//!
//! Conditions combine with AND. Text and field conditions can be inverted
//! (`exclude`). Text conditions look at the whole Entry, continuation lines
//! included, but only its first [`MAX_ENTRY_BYTES`] bytes.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fmt;
use std::io;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use regex::bytes::{Regex, RegexBuilder};
use serde_json::{Map, Value};

use crate::format::{Format, parse_entry};
use crate::index::{EntryIndex, EntryRule};
use crate::level::Level;
use crate::reader::BlockReader;
use crate::timestamp::TimestampStyle;

/// Largest Context a Filter may keep around each match.
pub const MAX_CONTEXT: usize = 50;
/// Only this many leading bytes of an Entry are examined.
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;

const CONTEXT_FLAG: u32 = 1 << 31;

/// Which Levels pass. Entries without a Level are governed by `include_none`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelFilter {
    allowed: [bool; 6],
    include_none: bool,
}

impl Default for LevelFilter {
    fn default() -> Self {
        Self { allowed: [true; 6], include_none: true }
    }
}

impl LevelFilter {
    /// "`level` and above".
    pub fn at_least(level: Level, include_none: bool) -> Self {
        let mut allowed = [false; 6];
        for l in Level::ALL.into_iter().filter(|l| *l >= level) {
            allowed[l as usize] = true;
        }
        Self { allowed, include_none }
    }

    pub fn only(levels: &[Level], include_none: bool) -> Self {
        let mut allowed = [false; 6];
        for &l in levels {
            allowed[l as usize] = true;
        }
        Self { allowed, include_none }
    }

    /// Whether this filter excludes anything at all.
    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }

    pub fn accepts(&self, level: Option<Level>) -> bool {
        match level {
            Some(l) => self.allowed[l as usize],
            None => self.include_none,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextPattern {
    Literal(String),
    Regex(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCondition {
    pub pattern: TextPattern,
    pub case_sensitive: bool,
    pub exclude: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldOp {
    /// Numbers compare numerically, strings exactly (case-sensitive).
    Equals(String),
    /// Case-sensitive substring of the field's text.
    Contains(String),
    Regex(String),
    Gt(f64),
    Ge(f64),
    Lt(f64),
    Le(f64),
    Exists,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldCondition {
    /// Dotted path into nested objects, e.g. `req.path`. A key that itself
    /// contains dots (`log.level`) is found as well.
    pub path: String,
    pub op: FieldOp,
    pub exclude: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    Text(TextCondition),
    Field(FieldCondition),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filter {
    pub levels: LevelFilter,
    /// Inclusive bounds. Entries without a usable timestamp never pass an
    /// active time range.
    pub time_from: Option<Timestamp>,
    pub time_to: Option<Timestamp>,
    pub conditions: Vec<Condition>,
    /// Entries kept before and after each match (clamped to [`MAX_CONTEXT`]).
    pub context: usize,
}

#[derive(Debug)]
pub struct FilterError(String);

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid filter: {}", self.0)
    }
}

impl std::error::Error for FilterError {}

/// Whether time-range Filtering can work for a file: syslog, time-only and
/// timestamp-less text carry no full date.
pub fn time_filter_available(format: Format, rule: EntryRule) -> bool {
    match format {
        Format::JsonLines => true,
        Format::PlainText => matches!(
            rule,
            EntryRule::Timestamped(TimestampStyle::Iso8601 | TimestampStyle::Apache)
        ),
    }
}

/// What the matcher needs to know about the file being filtered.
#[derive(Debug, Clone)]
pub struct FilterContext {
    pub format: Format,
    pub rule: EntryRule,
    pub zone: TimeZone,
}

enum CompiledOp {
    Equals(String),
    Contains(String),
    Regex(Regex),
    Compare(Ordering, bool, f64),
    Exists,
}

struct CompiledField {
    path: String,
    op: CompiledOp,
    exclude: bool,
}

pub struct CompiledFilter {
    levels: LevelFilter,
    time_from: Option<Timestamp>,
    time_to: Option<Timestamp>,
    texts: Vec<(Regex, bool)>,
    fields: Vec<CompiledField>,
    context: usize,
    needs_parse: bool,
}

fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex, FilterError> {
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| FilterError(e.to_string()))
}

impl CompiledFilter {
    pub fn new(filter: &Filter) -> Result<Self, FilterError> {
        let mut texts = Vec::new();
        let mut fields = Vec::new();
        for condition in &filter.conditions {
            match condition {
                Condition::Text(t) => {
                    let source = match &t.pattern {
                        TextPattern::Literal(s) => regex::escape(s),
                        TextPattern::Regex(s) => s.clone(),
                    };
                    texts.push((build_regex(&source, t.case_sensitive)?, t.exclude));
                }
                Condition::Field(f) => {
                    let op = match &f.op {
                        FieldOp::Equals(s) => CompiledOp::Equals(s.clone()),
                        FieldOp::Contains(s) => CompiledOp::Contains(s.clone()),
                        FieldOp::Regex(s) => CompiledOp::Regex(build_regex(s, true)?),
                        FieldOp::Gt(n) => CompiledOp::Compare(Ordering::Greater, false, *n),
                        FieldOp::Ge(n) => CompiledOp::Compare(Ordering::Greater, true, *n),
                        FieldOp::Lt(n) => CompiledOp::Compare(Ordering::Less, false, *n),
                        FieldOp::Le(n) => CompiledOp::Compare(Ordering::Less, true, *n),
                        FieldOp::Exists => CompiledOp::Exists,
                    };
                    fields.push(CompiledField { path: f.path.clone(), op, exclude: f.exclude });
                }
            }
        }
        let needs_parse = filter.levels.is_active()
            || filter.time_from.is_some()
            || filter.time_to.is_some()
            || !fields.is_empty();
        Ok(Self {
            levels: filter.levels.clone(),
            time_from: filter.time_from,
            time_to: filter.time_to,
            texts,
            fields,
            context: filter.context.min(MAX_CONTEXT),
            needs_parse,
        })
    }

    pub fn context(&self) -> usize {
        self.context
    }

    /// Whether `entry` (its bytes, continuation lines included) passes.
    pub fn matches(&self, entry: &[u8], ctx: &FilterContext) -> bool {
        let entry = &entry[..entry.len().min(MAX_ENTRY_BYTES)];
        if self.texts.iter().any(|(re, exclude)| re.is_match(entry) == *exclude) {
            return false;
        }
        if !self.needs_parse {
            return true;
        }
        let parsed = parse_entry(ctx.format, ctx.rule, entry, &ctx.zone);
        if !self.levels.accepts(parsed.level) {
            return false;
        }
        if self.time_from.is_some() || self.time_to.is_some() {
            let Some(t) = parsed.timestamp else { return false };
            if self.time_from.is_some_and(|from| t < from) || self.time_to.is_some_and(|to| t > to) {
                return false;
            }
        }
        self.fields.iter().all(|field| {
            let value = parsed.fields.as_ref().and_then(|map| get_path(map, &field.path));
            field.op.test(value) != field.exclude
        })
    }
}

fn get_path<'a>(map: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(v) = map.get(path) {
        return Some(v);
    }
    path.match_indices('.').find_map(|(i, _)| match map.get(&path[..i]) {
        Some(Value::Object(inner)) => get_path(inner, &path[i + 1..]),
        _ => None,
    })
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

fn number_of(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl CompiledOp {
    /// Test against a field; a missing field never matches.
    fn test(&self, value: Option<&Value>) -> bool {
        let Some(value) = value else { return false };
        match self {
            CompiledOp::Exists => true,
            CompiledOp::Equals(s) => match value {
                Value::String(v) => v == s,
                Value::Number(n) => s.trim().parse::<f64>().ok().is_some_and(|t| n.as_f64() == Some(t)),
                other => text_of(other) == *s,
            },
            CompiledOp::Contains(s) => text_of(value).contains(s.as_str()),
            CompiledOp::Regex(re) => re.is_match(text_of(value).as_bytes()),
            CompiledOp::Compare(want, or_equal, threshold) => number_of(value)
                .and_then(|n| n.partial_cmp(threshold))
                .is_some_and(|got| got == *want || (*or_equal && got == Ordering::Equal)),
        }
    }
}

/// A row of a [`FilteredView`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewRow {
    pub entry: usize,
    /// `false` for Context kept only because it surrounds a match.
    pub is_match: bool,
}

#[derive(Clone)]
struct Snapshot {
    next: usize,
    kept_len: usize,
    before: VecDeque<u32>,
    after_left: usize,
    matches: usize,
}

/// The Entries that survive a Filter, built incrementally as the index grows.
#[derive(Default)]
pub struct FilteredView {
    kept: Vec<u32>,
    next: usize,
    before: VecDeque<u32>,
    after_left: usize,
    matches: usize,
    /// State before the newest Entry, which may still be growing.
    snapshot: Option<Snapshot>,
}

impl FilteredView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.kept.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// Number of Entries that matched (Context excluded).
    pub fn match_count(&self) -> usize {
        self.matches
    }

    /// Number of Entries examined so far.
    pub fn examined(&self) -> usize {
        self.next
    }

    pub fn row(&self, row: usize) -> Option<ViewRow> {
        let raw = *self.kept.get(row)?;
        Some(ViewRow { entry: (raw & !CONTEXT_FLAG) as usize, is_match: raw & CONTEXT_FLAG == 0 })
    }

    /// Examines up to `max_entries` Entries and returns how many were new
    /// since the last call. The newest Entry is re-examined on every call
    /// because it may have grown. Call repeatedly until it returns 0.
    pub fn update(
        &mut self,
        filter: &CompiledFilter,
        index: &EntryIndex,
        reader: &BlockReader,
        ctx: &FilterContext,
        max_entries: usize,
    ) -> io::Result<usize> {
        let seen = self.next;
        if let Some(s) = self.snapshot.take() {
            self.kept.truncate(s.kept_len);
            self.next = s.next;
            self.before = s.before;
            self.after_left = s.after_left;
            self.matches = s.matches;
        }
        let end = index.len().min(self.next.saturating_add(max_entries));
        let mut buf = Vec::new();
        for i in self.next..end {
            if i + 1 == index.len() {
                self.snapshot = Some(Snapshot {
                    next: i,
                    kept_len: self.kept.len(),
                    before: self.before.clone(),
                    after_left: self.after_left,
                    matches: self.matches,
                });
            }
            let range = index.entry_range(i).expect("entry within index");
            let len = ((range.end - range.start) as usize).min(MAX_ENTRY_BYTES);
            buf.resize(len, 0);
            let n = reader.read_at(range.start, &mut buf)?;
            buf.truncate(n);
            self.process(i as u32, filter.matches(&buf, ctx), filter.context);
        }
        self.next = end;
        // The newest Entry is re-examined each call; count only new Entries.
        Ok(end.saturating_sub(seen))
    }

    fn process(&mut self, entry: u32, hit: bool, context: usize) {
        assert!(entry < CONTEXT_FLAG, "too many entries");
        if hit {
            self.kept.extend(self.before.drain(..).map(|e| e | CONTEXT_FLAG));
            self.kept.push(entry);
            self.matches += 1;
            self.after_left = context;
        } else if self.after_left > 0 {
            self.kept.push(entry | CONTEXT_FLAG);
            self.after_left -= 1;
        } else {
            self.before.push_back(entry);
            if self.before.len() > context {
                self.before.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn run(fx: &Fixture, filter: &Filter) -> Vec<ViewRow> {
        let compiled = CompiledFilter::new(filter).unwrap();
        let mut view = FilteredView::new();
        view.update(&compiled, &fx.index, &fx.reader, &fx.ctx, usize::MAX).unwrap();
        (0..view.len()).map(|r| view.row(r).unwrap()).collect()
    }

    fn entries(rows: &[ViewRow]) -> Vec<usize> {
        rows.iter().map(|r| r.entry).collect()
    }

    fn text(s: &str) -> Condition {
        Condition::Text(TextCondition {
            pattern: TextPattern::Literal(s.to_owned()),
            case_sensitive: true,
            exclude: false,
        })
    }

    fn not_text(s: &str) -> Condition {
        match text(s) {
            Condition::Text(mut t) => {
                t.exclude = true;
                Condition::Text(t)
            }
            c => c,
        }
    }

    const LOG: &str = "\
2026-10-18 19:50:01 INFO  server started
2026-10-18 19:50:02 INFO  healthcheck ok
2026-10-18 19:50:03 WARN  slow query
2026-10-18 19:50:04 ERROR request failed: timeout
    at handler.rs:42
    at router.rs:118
2026-10-18 19:50:05 INFO  healthcheck ok
2026-10-18 19:50:06 ERROR disk full
2026-10-18 19:50:07 INFO  done
";

    #[test]
    fn empty_filter_keeps_everything() {
        let fx = fixture(LOG);
        assert_eq!(entries(&run(&fx, &Filter::default())), (0..7).collect::<Vec<_>>());
    }

    #[test]
    fn text_matches_the_whole_entry_including_continuation_lines() {
        let fx = fixture(LOG);
        let f = Filter { conditions: vec![text("handler.rs:42")], ..Filter::default() };
        assert_eq!(entries(&run(&fx, &f)), vec![3]);
    }

    #[test]
    fn exclude_hides_entries_and_conditions_are_anded() {
        let fx = fixture(LOG);
        let f = Filter { conditions: vec![text("INFO"), not_text("healthcheck")], ..Filter::default() };
        assert_eq!(entries(&run(&fx, &f)), vec![0, 6]);
    }

    #[test]
    fn regex_and_case_insensitive_literals() {
        let fx = fixture(LOG);
        let re = Condition::Text(TextCondition {
            pattern: TextPattern::Regex("timeout|disk full".into()),
            case_sensitive: true,
            exclude: false,
        });
        assert_eq!(entries(&run(&fx, &Filter { conditions: vec![re], ..Filter::default() })), vec![3, 5]);
        let ci = Condition::Text(TextCondition {
            pattern: TextPattern::Literal("SLOW".into()),
            case_sensitive: false,
            exclude: false,
        });
        assert_eq!(entries(&run(&fx, &Filter { conditions: vec![ci], ..Filter::default() })), vec![2]);
    }

    #[test]
    fn invalid_regex_is_reported() {
        let bad = Condition::Text(TextCondition {
            pattern: TextPattern::Regex("(".into()),
            case_sensitive: true,
            exclude: false,
        });
        assert!(CompiledFilter::new(&Filter { conditions: vec![bad], ..Filter::default() }).is_err());
    }

    #[test]
    fn level_filter_with_and_without_unleveled_entries() {
        let fx = fixture(LOG);
        let f = Filter { levels: LevelFilter::at_least(Level::Warn, true), ..Filter::default() };
        assert_eq!(entries(&run(&fx, &f)), vec![2, 3, 5]);
        let only = Filter { levels: LevelFilter::only(&[Level::Error], true), ..Filter::default() };
        assert_eq!(entries(&run(&fx, &only)), vec![3, 5]);

        let plain = fixture("one\nERROR two\nthree\n");
        let with_none = Filter { levels: LevelFilter::at_least(Level::Error, true), ..Filter::default() };
        assert_eq!(entries(&run(&plain, &with_none)), vec![0, 1, 2]);
        let without = Filter { levels: LevelFilter::at_least(Level::Error, false), ..Filter::default() };
        assert_eq!(entries(&run(&plain, &without)), vec![1]);
    }

    #[test]
    fn time_range_is_inclusive_and_skips_entries_without_timestamps() {
        let fx = fixture(&format!("banner\n{LOG}"));
        let f = Filter {
            time_from: Some("2026-10-18T19:50:03Z".parse().unwrap()),
            time_to: Some("2026-10-18T19:50:05Z".parse().unwrap()),
            ..Filter::default()
        };
        // Entry 0 is the preamble (no timestamp); 3..=5 fall in range.
        assert_eq!(entries(&run(&fx, &f)), vec![3, 4, 5]);
        assert!(time_filter_available(fx.ctx.format, fx.ctx.rule));
        assert!(!time_filter_available(Format::PlainText, EntryRule::PerLine));
        assert!(!time_filter_available(
            Format::PlainText,
            EntryRule::Timestamped(TimestampStyle::Syslog)
        ));
    }

    const JSON: &str = "\
{\"level\":\"info\",\"user_id\":42,\"duration_ms\":120,\"req\":{\"path\":\"/api/items\"}}
{\"level\":\"error\",\"user_id\":7,\"duration_ms\":900,\"req\":{\"path\":\"/api/pay\"},\"log.level\":\"x\"}
{\"level\":\"warn\",\"user_id\":42,\"duration_ms\":501.5,\"req\":{\"path\":\"/health\"}}
{\"level\":\"info\",\"msg\":\"no fields\"}
";

    fn field(path: &str, op: FieldOp, exclude: bool) -> Condition {
        Condition::Field(FieldCondition { path: path.into(), op, exclude })
    }

    #[test]
    fn field_conditions_on_json() {
        let fx = fixture(JSON);
        let run_one = |c| entries(&run(&fx, &Filter { conditions: vec![c], ..Filter::default() }));
        assert_eq!(run_one(field("user_id", FieldOp::Equals("42".into()), false)), vec![0, 2]);
        assert_eq!(run_one(field("duration_ms", FieldOp::Gt(500.0), false)), vec![1, 2]);
        assert_eq!(run_one(field("duration_ms", FieldOp::Le(120.0), false)), vec![0]);
        assert_eq!(run_one(field("req.path", FieldOp::Contains("/api".into()), false)), vec![0, 1]);
        assert_eq!(run_one(field("req.path", FieldOp::Regex("^/(pay|health)".into()), false)), vec![2]);
        assert_eq!(run_one(field("log.level", FieldOp::Exists, false)), vec![1]);
        // A missing field never matches, so excluding keeps those Entries.
        assert_eq!(run_one(field("user_id", FieldOp::Equals("42".into()), true)), vec![1, 3]);
    }

    #[test]
    fn field_conditions_never_match_plain_text() {
        let fx = fixture(LOG);
        let f = Filter { conditions: vec![field("a", FieldOp::Exists, false)], ..Filter::default() };
        assert!(run(&fx, &f).is_empty());
    }

    #[test]
    fn level_filter_applies_to_json_lines() {
        let fx = fixture(JSON);
        let f = Filter { levels: LevelFilter::at_least(Level::Warn, true), ..Filter::default() };
        assert_eq!(entries(&run(&fx, &f)), vec![1, 2]);
    }

    #[test]
    fn context_surrounds_matches_without_duplicates() {
        let fx = fixture(LOG);
        let f = Filter { conditions: vec![text("ERROR")], context: 1, ..Filter::default() };
        let rows = run(&fx, &f);
        assert_eq!(entries(&rows), vec![2, 3, 4, 5, 6]);
        let flags: Vec<bool> = rows.iter().map(|r| r.is_match).collect();
        assert_eq!(flags, vec![false, true, false, true, false]);
    }

    #[test]
    fn context_is_clamped() {
        let f = Filter { context: 10_000, ..Filter::default() };
        assert_eq!(CompiledFilter::new(&f).unwrap().context(), MAX_CONTEXT);
    }

    #[test]
    fn incremental_updates_match_a_single_pass() {
        let fx = fixture(&LOG.repeat(50));
        let f = Filter { conditions: vec![text("ERROR")], context: 2, ..Filter::default() };
        let whole = run(&fx, &f);
        let compiled = CompiledFilter::new(&f).unwrap();
        let mut view = FilteredView::new();
        while view.update(&compiled, &fx.index, &fx.reader, &fx.ctx, 3).unwrap() > 0 {}
        let stepped: Vec<ViewRow> = (0..view.len()).map(|r| view.row(r).unwrap()).collect();
        assert_eq!(stepped, whole);
        assert_eq!(view.match_count(), 100);
    }

    #[test]
    fn the_newest_entry_is_reexamined_as_it_grows() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 INFO b\n").unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let (format, mut index) = EntryIndex::open(&reader, None).unwrap();
        let ctx = FilterContext { format, rule: index.rule(), zone: TimeZone::UTC };
        index.advance(&reader, u64::MAX).unwrap();
        let f = CompiledFilter::new(&Filter { conditions: vec![text("ERROR")], ..Filter::default() }).unwrap();
        let mut view = FilteredView::new();
        view.update(&f, &index, &reader, &ctx, usize::MAX).unwrap();
        assert_eq!(view.len(), 0);

        // The last Entry gains a continuation line containing the match.
        file.write_all(b"    caused by ERROR\n").unwrap();
        file.flush().unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        view.update(&f, &index, &reader, &ctx, usize::MAX).unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(view.row(0).unwrap().entry, 1);

        // Updating again without changes must not duplicate it.
        view.update(&f, &index, &reader, &ctx, usize::MAX).unwrap();
        assert_eq!(view.len(), 1);
        assert_eq!(view.match_count(), 1);
    }
}
