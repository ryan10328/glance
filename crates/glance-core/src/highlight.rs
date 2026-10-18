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

/// A palette color: name, value on a light background, value on a dark one.
pub type PaletteColor = (&'static str, Rgb, Rgb);

/// The colors a rule can pick from.
pub const PALETTE: [PaletteColor; 8] = [
    ("Red", [0xdc, 0x26, 0x26], [0xf8, 0x71, 0x71]),
    ("Orange", [0xea, 0x58, 0x0c], [0xfb, 0x92, 0x3c]),
    ("Yellow", [0xca, 0x8a, 0x04], [0xfa, 0xcc, 0x15]),
    ("Green", [0x16, 0xa3, 0x4a], [0x4a, 0xde, 0x80]),
    ("Cyan", [0x08, 0x91, 0xb2], [0x22, 0xd3, 0xee]),
    ("Blue", [0x25, 0x63, 0xeb], [0x60, 0xa5, 0xfa]),
    ("Purple", [0x93, 0x33, 0xea], [0xc0, 0x84, 0xfc]),
    ("Pink", [0xdb, 0x27, 0x77], [0xf4, 0x72, 0xb6]),
];

/// The color to draw palette entry `index` in.
pub fn palette_rgb(index: usize, dark: bool) -> Rgb {
    let (_, light, dark_rgb) = PALETTE[index.min(PALETTE.len() - 1)];
    if dark { dark_rgb } else { light }
}

struct CompiledRule {
    regex: Regex,
    /// Index into [`PALETTE`].
    color: usize,
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
                Ok(regex) => compiled.push(CompiledRule { regex, color: rule.color.min(PALETTE.len() - 1) }),
                Err(e) => problems.push((i, e.to_string())),
            }
        }
        (Self { rules: compiled }, problems)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Matches in `line` with their palette index, ascending and not
    /// overlapping. Where two rules overlap the earlier rule keeps the text.
    pub fn text_spans(&self, line: &[u8]) -> Vec<(Range<usize>, usize)> {
        let mut spans: Vec<(Range<usize>, usize)> = Vec::new();
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
        assert_eq!(h.text_spans(b"it is ok"), vec![(6..8, 1)]);
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
        assert_eq!(spans, vec![(0..6, 5), (13..27, 0)]);
    }

    #[test]
    fn spans_do_not_overlap_and_earlier_rules_win() {
        let (h, _) = Highlighter::new(&[rule("slow query", false, 0), rule("query", false, 3), rule("user_\\d+", true, 5)]);
        let spans = h.text_spans(b"user_7 ran a slow query, then another query");
        assert_eq!(spans, vec![(0..6, 5), (13..23, 0), (38..43, 3)]);
    }

    #[test]
    fn an_out_of_range_color_falls_back_to_the_last_one() {
        let (h, _) = Highlighter::new(&[rule("x", false, 99)]);
        assert_eq!(h.text_spans(b"x")[0].1, 7);
    }

    #[test]
    fn every_palette_color_has_a_light_and_a_dark_variant() {
        for (i, (name, light, dark)) in PALETTE.iter().enumerate() {
            assert_ne!(light, dark, "{name}");
            assert_eq!(palette_rgb(i, false), *light);
            assert_eq!(palette_rgb(i, true), *dark);
        }
    }
}
