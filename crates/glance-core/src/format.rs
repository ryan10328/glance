//! Formats and Entry parsing: turning an Entry's raw bytes into a timestamp,
//! a Level, a message and (for JSON lines) structured fields.
//!
//! Displayed text is never altered; everything here is derived data.

use jiff::Timestamp;
use jiff::civil;
use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use crate::index::EntryRule;
use crate::level::Level;
use crate::timestamp::TimestampStyle;

const DETECT_LINES: usize = 50;
/// Share of sampled lines that must be JSON objects to call a file JSON lines.
const JSON_THRESHOLD: f64 = 0.8;

const TIMESTAMP_KEYS: [&str; 5] = ["timestamp", "time", "ts", "@timestamp", "datetime"];
const LEVEL_KEYS: [&str; 4] = ["level", "severity", "lvl", "log.level"];
const MESSAGE_KEYS: [&str; 3] = ["message", "msg", "text"];
/// How many tokens after the timestamp may hold a plain-text Level.
const LEVEL_TOKENS: usize = 3;

/// The structure shared by every line of a Log File.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    PlainText,
    JsonLines,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::PlainText, Format::JsonLines];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::PlainText => "plain text",
            Format::JsonLines => "JSON lines",
        }
    }

    /// Format Detection from the start of a file. `truncated` says the sample
    /// was cut off, so its last line may be incomplete and is ignored.
    pub fn detect(sample: &[u8], truncated: bool) -> Format {
        let mut lines: Vec<&[u8]> = sample.split(|&b| b == b'\n').collect();
        if truncated {
            lines.pop();
        }
        let lines: Vec<&[u8]> = lines
            .into_iter()
            .map(trim_ascii)
            .filter(|l| !l.is_empty())
            .take(DETECT_LINES)
            .collect();
        if lines.is_empty() {
            return Format::PlainText;
        }
        let json = lines.iter().filter(|l| is_json_object(l)).count();
        if json as f64 / lines.len() as f64 >= JSON_THRESHOLD {
            Format::JsonLines
        } else {
            Format::PlainText
        }
    }

    /// How lines group into Entries under this Format.
    pub fn entry_rule(self, sample: &[u8]) -> EntryRule {
        match self {
            Format::JsonLines => EntryRule::PerLine,
            Format::PlainText => EntryRule::detect(sample),
        }
    }
}

fn trim_ascii(line: &[u8]) -> &[u8] {
    line.trim_ascii()
}

fn is_json_object(line: &[u8]) -> bool {
    line.first() == Some(&b'{') && matches!(serde_json::from_slice::<Value>(line), Ok(Value::Object(_)))
}

/// Chooses the time zone for timestamps without one: a per-file choice, then
/// the configured default, then the local zone.
pub fn resolve_time_zone(file: Option<&str>, configured: Option<&str>) -> Result<TimeZone, jiff::Error> {
    match file.or(configured) {
        Some(name) => TimeZone::get(name),
        None => Ok(TimeZone::system()),
    }
}

/// Derived data for one Entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedEntry {
    /// `None` for timestamps that cannot be placed in time (syslog and
    /// time-only styles carry no full date) or when there is no timestamp.
    pub timestamp: Option<Timestamp>,
    pub level: Option<Level>,
    pub message: Option<String>,
    /// All JSON fields; `None` for plain text.
    pub fields: Option<Map<String, Value>>,
}

/// Parses one Entry. `entry` is the Entry's bytes (any continuation lines
/// included); only its first line is examined. `zone` applies to timestamps
/// that carry none.
pub fn parse_entry(format: Format, rule: EntryRule, entry: &[u8], zone: &TimeZone) -> ParsedEntry {
    let first_line = entry.split(|&b| b == b'\n').next().unwrap_or_default();
    let first_line = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    match format {
        Format::JsonLines => parse_json(first_line, zone),
        Format::PlainText => parse_plain(rule, first_line, zone),
    }
}

fn parse_json(line: &[u8], zone: &TimeZone) -> ParsedEntry {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(line.trim_ascii()) else {
        return ParsedEntry::default();
    };
    let timestamp = TIMESTAMP_KEYS.iter().find_map(|k| fields.get(*k)).and_then(|v| json_timestamp(v, zone));
    let level = LEVEL_KEYS.iter().find_map(|k| fields.get(*k)).and_then(|v| match v {
        Value::String(s) => Level::parse(s),
        Value::Number(n) => n.as_i64().and_then(Level::from_number),
        _ => None,
    });
    let message = MESSAGE_KEYS.iter().find_map(|k| fields.get(*k)).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    });
    ParsedEntry { timestamp, level, message, fields: Some(fields) }
}

fn json_timestamp(value: &Value, zone: &TimeZone) -> Option<Timestamp> {
    match value {
        Value::String(s) => parse_iso(s.as_bytes(), zone).map(|(t, _)| t),
        Value::Number(n) => {
            // Epoch seconds, or milliseconds when the magnitude says so.
            let seconds = n.as_f64()?;
            let millis = if seconds.abs() >= 1e11 { seconds } else { seconds * 1000.0 };
            Timestamp::from_millisecond(millis as i64).ok()
        }
        _ => None,
    }
}

fn parse_plain(rule: EntryRule, line: &[u8], zone: &TimeZone) -> ParsedEntry {
    let (timestamp, skip_tokens) = match rule {
        EntryRule::Timestamped(style) if style.matches(line) => {
            let timestamp = match style {
                TimestampStyle::Iso8601 => parse_iso(strip_bracket(line), zone).map(|(t, _)| t),
                TimestampStyle::Apache => parse_apache(line),
                TimestampStyle::Syslog | TimestampStyle::TimeOnly => None,
            };
            (timestamp, timestamp_tokens(style, line))
        }
        _ => (None, 0),
    };
    let text = String::from_utf8_lossy(line);
    let level = text.split_whitespace().skip(skip_tokens).take(LEVEL_TOKENS).find_map(plain_level);
    // The message is everything after the timestamp tokens.
    let mut rest = text.as_ref();
    for _ in 0..skip_tokens {
        rest = rest.trim_start().split_once(char::is_whitespace).map_or("", |(_, r)| r);
    }
    ParsedEntry { timestamp, level, message: Some(rest.trim().to_owned()), fields: None }
}

fn plain_level(token: &str) -> Option<Level> {
    match token.split_once('=') {
        Some((key, value)) => {
            matches!(key.to_ascii_lowercase().as_str(), "level" | "lvl" | "severity")
                .then(|| Level::parse(value.trim_matches('"')))
                .flatten()
        }
        None => Level::parse(token),
    }
}

/// How many whitespace-separated tokens the timestamp occupies.
fn timestamp_tokens(style: TimestampStyle, line: &[u8]) -> usize {
    match style {
        TimestampStyle::Syslog => 3,
        TimestampStyle::TimeOnly => 1,
        TimestampStyle::Apache => 2,
        TimestampStyle::Iso8601 => {
            let body = strip_bracket(line);
            if body.get(10) == Some(&b' ') { 2 } else { 1 }
        }
    }
}

fn strip_bracket(line: &[u8]) -> &[u8] {
    line.strip_prefix(b"[").unwrap_or(line)
}

/// Parses an ISO-8601-like timestamp at the start of `s`. Returns the instant
/// and the number of bytes consumed. Timestamps without an offset are read in
/// `zone`.
fn parse_iso(s: &[u8], zone: &TimeZone) -> Option<(Timestamp, usize)> {
    if s.len() < 19 {
        return None;
    }
    let mut text = String::with_capacity(35);
    for (i, &b) in s[..10].iter().enumerate() {
        text.push(if i == 4 || i == 7 { '-' } else { b as char });
    }
    if !matches!(s[10], b'T' | b' ' | b't') {
        return None;
    }
    text.push('T');
    let mut end = 11;
    while end < s.len() && matches!(s[end], b'0'..=b'9' | b':' | b'.' | b',') {
        text.push(if s[end] == b',' { '.' } else { s[end] as char });
        end += 1;
    }
    let mut has_offset = false;
    match s.get(end) {
        Some(b'Z' | b'z') => {
            text.push('Z');
            end += 1;
            has_offset = true;
        }
        Some(&sign @ (b'+' | b'-')) => {
            let digits: String = s[end + 1..].iter().take_while(|b| b.is_ascii_digit() || **b == b':').map(|&b| b as char).collect();
            let compact: String = digits.chars().filter(char::is_ascii_digit).collect();
            if compact.len() == 2 || compact.len() == 4 {
                text.push(sign as char);
                text.push_str(&compact[..2]);
                text.push(':');
                text.push_str(if compact.len() == 4 { &compact[2..] } else { "00" });
                end += 1 + digits.len();
                has_offset = true;
            }
        }
        _ => {}
    }
    let instant = if has_offset {
        text.parse::<Timestamp>().ok()?
    } else {
        text.parse::<civil::DateTime>().ok()?.to_zoned(zone.clone()).ok()?.timestamp()
    };
    Some((instant, end))
}

fn parse_apache(line: &[u8]) -> Option<Timestamp> {
    let text = std::str::from_utf8(line).ok()?;
    let inner = text.strip_prefix('[')?;
    let end = inner.find(']')?;
    jiff::fmt::strtime::parse("%d/%b/%Y:%H:%M:%S %z", &inner[..end])
        .ok()?
        .to_timestamp()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn plain(rule: EntryRule, line: &str) -> ParsedEntry {
        parse_entry(Format::PlainText, rule, line.as_bytes(), &utc())
    }

    const ISO: EntryRule = EntryRule::Timestamped(TimestampStyle::Iso8601);

    #[test]
    fn detects_json_lines_and_plain_text() {
        let json = b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n";
        assert_eq!(Format::detect(json, false), Format::JsonLines);
        let text = b"2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 INFO b\n";
        assert_eq!(Format::detect(text, false), Format::PlainText);
        assert_eq!(Format::detect(b"", false), Format::PlainText);
        // A JSON array or scalar per line is not a JSON object.
        assert_eq!(Format::detect(b"[1,2]\n[3]\n", false), Format::PlainText);
    }

    #[test]
    fn truncated_sample_ignores_the_cut_last_line() {
        let sample = b"{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n{\"a\":";
        assert_eq!(Format::detect(sample, true), Format::JsonLines);
        let mixed = b"{\"a\":1}\nnot json\nnot json\nnot json\n";
        assert_eq!(Format::detect(mixed, false), Format::PlainText);
    }

    #[test]
    fn json_fields_use_the_common_key_names() {
        let line = br#"{"@timestamp":"2026-10-18T19:50:01Z","severity":"err","msg":"boom","user_id":42}"#;
        let e = parse_entry(Format::JsonLines, EntryRule::PerLine, line, &utc());
        assert_eq!(e.timestamp, Some("2026-10-18T19:50:01Z".parse().unwrap()));
        assert_eq!(e.level, Some(Level::Error));
        assert_eq!(e.message.as_deref(), Some("boom"));
        assert_eq!(e.fields.unwrap()["user_id"], 42);
    }

    #[test]
    fn json_numeric_time_and_level() {
        let e = parse_entry(Format::JsonLines, EntryRule::PerLine, br#"{"time":1760817001,"level":50}"#, &utc());
        assert_eq!(e.timestamp.unwrap().as_second(), 1760817001);
        assert_eq!(e.level, Some(Level::Error));
        let ms = parse_entry(Format::JsonLines, EntryRule::PerLine, br#"{"ts":1760817001123}"#, &utc());
        assert_eq!(ms.timestamp.unwrap().as_millisecond(), 1760817001123);
    }

    #[test]
    fn json_without_known_keys_keeps_only_fields() {
        let e = parse_entry(Format::JsonLines, EntryRule::PerLine, br#"{"foo":"bar"}"#, &utc());
        assert!(e.timestamp.is_none() && e.level.is_none() && e.message.is_none());
        assert_eq!(e.fields.unwrap()["foo"], "bar");
        let bad = parse_entry(Format::JsonLines, EntryRule::PerLine, b"{broken", &utc());
        assert_eq!(bad, ParsedEntry::default());
    }

    #[test]
    fn plain_text_level_follows_the_timestamp() {
        let e = plain(ISO, "2026-10-18 19:50:07 ERROR request failed");
        assert_eq!(e.level, Some(Level::Error));
        assert_eq!(e.message.as_deref(), Some("ERROR request failed"));
        assert_eq!(plain(ISO, "2026-10-18 19:50:07 [main] WARN  c.a.Foo - slow").level, Some(Level::Warn));
        assert_eq!(plain(ISO, "[2026-10-18 19:50:07] [info] ok").level, Some(Level::Info));
        assert_eq!(plain(ISO, "2026-10-18T19:50:07Z level=error msg=x").level, Some(Level::Error));
    }

    #[test]
    fn a_level_word_deep_in_the_message_is_ignored() {
        let e = plain(ISO, "2026-10-18 19:50:07 request done and there was no error found");
        assert_eq!(e.level, None);
    }

    #[test]
    fn lines_without_timestamps_look_at_their_first_tokens() {
        let e = plain(EntryRule::PerLine, "ERROR disk full");
        assert_eq!(e.level, Some(Level::Error));
        assert_eq!(e.timestamp, None);
        assert_eq!(plain(EntryRule::PerLine, "just some text").level, None);
    }

    #[test]
    fn iso_timestamps_with_and_without_zone() {
        let z = plain(ISO, "2026-10-18T19:50:01Z INFO x").timestamp.unwrap();
        let offset = plain(ISO, "2026-10-18 19:50:01+02:00 INFO x").timestamp.unwrap();
        assert_eq!(z.as_second() - offset.as_second(), 2 * 3600);
        let compact = plain(ISO, "2026-10-18 19:50:01,250+0200 INFO x").timestamp.unwrap();
        assert_eq!(compact.as_millisecond() - offset.as_millisecond(), 250);

        let naive = |zone: &TimeZone| {
            parse_entry(Format::PlainText, ISO, b"2026-10-18 19:50:01 INFO x", zone).timestamp.unwrap()
        };
        let taipei = TimeZone::get("Asia/Taipei").unwrap();
        assert_eq!(naive(&utc()).as_second() - naive(&taipei).as_second(), 8 * 3600);
    }

    #[test]
    fn apache_timestamps_parse_and_syslog_has_none() {
        let apache = EntryRule::Timestamped(TimestampStyle::Apache);
        let e = plain(apache, "[18/Oct/2026:19:50:01 +0000] \"GET /\" 200");
        assert_eq!(e.timestamp, Some("2026-10-18T19:50:01Z".parse().unwrap()));

        let syslog = EntryRule::Timestamped(TimestampStyle::Syslog);
        let e = plain(syslog, "Oct 18 19:50:01 host sshd[1]: error: bad");
        assert_eq!(e.timestamp, None);
        assert_eq!(e.level, Some(Level::Error));
        assert_eq!(e.message.as_deref(), Some("host sshd[1]: error: bad"));
    }

    #[test]
    fn only_the_first_line_of_an_entry_is_examined() {
        let entry = b"2026-10-18 19:50:07 INFO ok\n    ERROR in the trace\r\n";
        let e = parse_entry(Format::PlainText, ISO, entry, &utc());
        assert_eq!(e.level, Some(Level::Info));
    }

    #[test]
    fn time_zone_resolution_order() {
        let taipei = resolve_time_zone(Some("Asia/Taipei"), Some("UTC")).unwrap();
        assert_eq!(taipei.iana_name(), Some("Asia/Taipei"));
        let configured = resolve_time_zone(None, Some("UTC")).unwrap();
        assert_eq!(configured.iana_name(), Some("UTC"));
        assert!(resolve_time_zone(None, None).is_ok());
        assert!(resolve_time_zone(Some("Not/AZone"), None).is_err());
    }
}
