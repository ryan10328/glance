//! Recognizing the timestamp that opens an Entry in a plain-text log.

/// Number of leading bytes of a line that any style needs to decide.
pub const PREFIX_LEN: usize = 40;

const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// Timestamp shapes, in priority order (most specific first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampStyle {
    /// `[18/Oct/2026:19:50:01 +0000]` (Apache, nginx)
    Apache,
    /// `2026-10-18T19:50:01Z`, `2026-10-18 19:50:01,123`, `2026/10/18 19:50:01`,
    /// optionally wrapped in `[` `]`.
    Iso8601,
    /// `Oct 18 19:50:01`, `Oct  8 19:50:01`
    Syslog,
    /// `19:50:01.123`, optionally wrapped in `[` `]`.
    TimeOnly,
}

impl TimestampStyle {
    pub const ALL: [TimestampStyle; 4] = [
        TimestampStyle::Apache,
        TimestampStyle::Iso8601,
        TimestampStyle::Syslog,
        TimestampStyle::TimeOnly,
    ];

    /// Whether `line` (or its first bytes) starts with this style's timestamp.
    pub fn matches(self, line: &[u8]) -> bool {
        match self {
            TimestampStyle::Apache => apache(line),
            TimestampStyle::Iso8601 => iso8601(strip_bracket(line)),
            TimestampStyle::Syslog => syslog(line),
            TimestampStyle::TimeOnly => clock(strip_bracket(line)),
        }
    }

    /// Picks the style that opens the most lines among `lines`; ties go to the
    /// higher-priority style. `None` if no line matches any style.
    pub fn detect<'a>(lines: impl IntoIterator<Item = &'a [u8]>) -> Option<TimestampStyle> {
        let mut counts = [0usize; 4];
        for line in lines {
            for (i, style) in Self::ALL.iter().enumerate() {
                if style.matches(line) {
                    counts[i] += 1;
                    break;
                }
            }
        }
        let (best, &count) = counts.iter().enumerate().max_by_key(|&(i, &c)| (c, std::cmp::Reverse(i)))?;
        (count > 0).then(|| Self::ALL[best])
    }
}

fn strip_bracket(line: &[u8]) -> &[u8] {
    line.strip_prefix(b"[").unwrap_or(line)
}

fn digits(s: &[u8], n: usize) -> Option<&[u8]> {
    (s.len() >= n && s[..n].iter().all(u8::is_ascii_digit)).then(|| &s[n..])
}

/// `dd:dd:dd`
fn clock(s: &[u8]) -> bool {
    digits(s, 2)
        .and_then(|s| s.strip_prefix(b":"))
        .and_then(|s| digits(s, 2))
        .and_then(|s| s.strip_prefix(b":"))
        .and_then(|s| digits(s, 2))
        .is_some()
}

/// `dddd-dd-dd[T ]dd:dd:dd` (also `/` as the date separator)
fn iso8601(s: &[u8]) -> bool {
    let Some(s) = digits(s, 4) else { return false };
    let Some(sep) = s.first().filter(|c| matches!(c, b'-' | b'/')) else { return false };
    let s = &s[1..];
    let Some(s) = digits(s, 2) else { return false };
    if s.first() != Some(sep) {
        return false;
    }
    let Some(s) = digits(&s[1..], 2) else { return false };
    match s.first() {
        Some(b'T' | b' ') => clock(&s[1..]),
        _ => false,
    }
}

/// `Mon d[d] dd:dd:dd` where a single-digit day may be space-padded.
fn syslog(s: &[u8]) -> bool {
    if s.len() < 3 || !MONTHS.iter().any(|m| m.as_slice() == &s[..3]) {
        return false;
    }
    let Some(mut s) = s[3..].strip_prefix(b" ") else { return false };
    if let Some(rest) = s.strip_prefix(b" ") {
        s = rest;
    }
    let day = s.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&day) {
        return false;
    }
    match s[day..].strip_prefix(b" ") {
        Some(rest) => clock(rest),
        None => false,
    }
}

/// `[dd/Mon/dddd:dd:dd:dd`
fn apache(s: &[u8]) -> bool {
    let Some(s) = s.strip_prefix(b"[") else { return false };
    let Some(s) = digits(s, 2).and_then(|s| s.strip_prefix(b"/")) else { return false };
    if s.len() < 3 || !MONTHS.iter().any(|m| m.as_slice() == &s[..3]) {
        return false;
    }
    match s[3..].strip_prefix(b"/").and_then(|s| digits(s, 4)).and_then(|s| s.strip_prefix(b":")) {
        Some(rest) => clock(rest),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TimestampStyle::*;

    #[test]
    fn recognizes_each_style() {
        assert!(Iso8601.matches(b"2026-10-18T19:50:01Z started"));
        assert!(Iso8601.matches(b"2026-10-18 19:50:01,123 INFO x"));
        assert!(Iso8601.matches(b"[2026-10-18 19:50:01] INFO x"));
        assert!(Iso8601.matches(b"2026/10/18 19:50:01 x"));
        assert!(Syslog.matches(b"Oct 18 19:50:01 host sshd[1]: x"));
        assert!(Syslog.matches(b"Oct  8 19:50:01 host x"));
        assert!(Apache.matches(b"[18/Oct/2026:19:50:01 +0000] GET /"));
        assert!(TimeOnly.matches(b"19:50:01.123 hello"));
        assert!(TimeOnly.matches(b"[19:50:01] hello"));
    }

    #[test]
    fn rejects_non_timestamps() {
        for line in [&b""[..], b"    at handler.rs:42", b"2026-10-18", b"Octo 18 19:50:01", b"19:5:01", b"{\"a\":1}"] {
            for style in TimestampStyle::ALL {
                assert!(!style.matches(line), "{style:?} matched {:?}", String::from_utf8_lossy(line));
            }
        }
    }

    #[test]
    fn detect_picks_the_most_common_style() {
        let lines: [&[u8]; 5] = [
            b"=== banner ===",
            b"2026-10-18 19:50:01 INFO a",
            b"    at x",
            b"2026-10-18 19:50:02 ERROR b",
            b"19:50:03 odd",
        ];
        assert_eq!(TimestampStyle::detect(lines), Some(Iso8601));
        assert_eq!(TimestampStyle::detect([&b"hello"[..], b"world"]), None);
    }
}
