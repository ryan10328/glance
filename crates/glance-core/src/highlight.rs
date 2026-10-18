//! Highlight Rules: text the user wants to stand out, compiled for the Entry view.
//!
//! A rule colors the text it matches (the text color, not the background, so
//! Level colors stay readable). Rules are tried top to bottom; where two rules
//! overlap the earlier one keeps the text. Matching looks at the first line of
//! an Entry, the part the Entry view shows.

use std::ops::Range;

use regex::bytes::Regex;

use crate::filter::{TextPattern, build_regex};
use crate::settings::HighlightRule;

/// A color as red, green, blue.
pub type Rgb = [u8; 3];

/// The colors a rule can pick from: name and value.
pub const PALETTE: [(&str, Rgb); 8] = [
    ("Red", [0xef, 0x44, 0x44]),
    ("Orange", [0xf9, 0x73, 0x16]),
    ("Yellow", [0xca, 0x8a, 0x04]),
    ("Green", [0x22, 0xc5, 0x5e]),
    ("Cyan", [0x06, 0xb6, 0xd4]),
    ("Blue", [0x3b, 0x82, 0xf6]),
    ("Purple", [0xa8, 0x55, 0xf7]),
    ("Pink", [0xec, 0x48, 0x99]),
];

struct CompiledRule {
    regex: Regex,
    color: Rgb,
}

#[derive(Default)]
pub struct Highlighter {
    rules: Vec<CompiledRule>,
}

/// Why a rule cannot be used, by its position in the list.
pub type Problem = (usize, String);

impl Highlighter {
    /// Compiles the rules. A rule with an empty pattern is simply inactive; one
    /// that cannot be compiled (a half-typed regex, say) is skipped and
    /// reported, so it does not switch off the others.
    pub fn new(rules: &[HighlightRule]) -> (Self, Vec<Problem>) {
        let mut compiled = Vec::new();
        let mut problems = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if rule.pattern.is_empty() {
                continue;
            }
            let pattern = if rule.regex { TextPattern::Regex(rule.pattern.clone()) } else { TextPattern::Wildcard(rule.pattern.clone()) };
            match build_regex(&pattern.to_regex_source(), rule.case_sensitive) {
                Ok(regex) => compiled.push(CompiledRule { regex, color: PALETTE[rule.color.min(PALETTE.len() - 1)].1 }),
                Err(e) => problems.push((i, e.to_string())),
            }
        }
        (Self { rules: compiled }, problems)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Matches in `line`, ascending and not overlapping. Where two rules
    /// overlap the earlier rule keeps the text.
    pub fn text_spans(&self, line: &[u8]) -> Vec<(Range<usize>, Rgb)> {
        let mut spans: Vec<(Range<usize>, Rgb)> = Vec::new();
        for rule in &self.rules {
            for m in rule.regex.find_iter(line).filter(|m| !m.is_empty()) {
                let range = m.range();
                if spans.iter().all(|(taken, _)| range.end <= taken.start || taken.end <= range.start) {
                    spans.push((range, rule.color));
                }
            }
        }
        spans.sort_by_key(|(range, _)| range.start);
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, regex: bool, color: usize) -> HighlightRule {
        HighlightRule { pattern: pattern.into(), regex, case_sensitive: false, color }
    }

    #[test]
    fn empty_rules_are_inactive_and_bad_ones_are_reported() {
        let rules = [rule("", false, 0), rule("(", true, 0), rule("ok", false, 1)];
        let (h, problems) = Highlighter::new(&rules);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].0, 1);
        assert_eq!(h.text_spans(b"it is ok"), vec![(6..8, PALETTE[1].1)]);
    }

    #[test]
    fn matching_ignores_case_unless_asked() {
        let mut sensitive = rule("Timeout", false, 0);
        sensitive.case_sensitive = true;
        let (h, _) = Highlighter::new(&[sensitive]);
        assert_eq!(h.text_spans(b"a TIMEOUT, a Timeout").len(), 1);
        let (h, _) = Highlighter::new(&[rule("timeout", false, 0)]);
        assert_eq!(h.text_spans(b"a TIMEOUT, a Timeout").len(), 2);
    }

    #[test]
    fn wildcards_and_regexes_both_work() {
        let (h, _) = Highlighter::new(&[rule("slow*query", false, 0), rule(r"user_\d+", true, 5)]);
        let spans = h.text_spans(b"user_7 ran a slow big query");
        assert_eq!(spans, vec![(0..6, PALETTE[5].1), (13..27, PALETTE[0].1)]);
    }

    #[test]
    fn spans_do_not_overlap_and_earlier_rules_win() {
        let (h, _) = Highlighter::new(&[rule("slow query", false, 0), rule("query", false, 3), rule("user_\\d+", true, 5)]);
        let spans = h.text_spans(b"user_7 ran a slow query, then another query");
        assert_eq!(spans, vec![(0..6, PALETTE[5].1), (13..23, PALETTE[0].1), (38..43, PALETTE[3].1)]);
    }

    #[test]
    fn an_out_of_range_color_falls_back_to_the_last_one() {
        let (h, _) = Highlighter::new(&[rule("x", false, 99)]);
        assert_eq!(h.text_spans(b"x")[0].1, PALETTE[7].1);
    }
}
