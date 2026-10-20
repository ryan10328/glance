//! Reading Entries for display and for copying.

use std::fmt;
use std::io;

use crate::format::Format;
use crate::index::EntryIndex;
use crate::reader::BlockReader;

/// Bytes of a first line shown in the Entry view; the rest is cut with `…`.
pub const MAX_DISPLAY_BYTES: usize = 4096;
/// How much of an Entry is read to summarize it.
const SUMMARY_READ_BYTES: usize = 64 * 1024;

/// How much of one Entry the detail panel reads.
pub const DETAIL_READ_BYTES: usize = 256 * 1024;

/// One Entry in full, for the detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryDetail {
    pub text: String,
    /// Size of the Entry in the file, in bytes.
    pub size: usize,
    /// The Entry was longer than [`DETAIL_READ_BYTES`], so `text` stops early.
    pub truncated: bool,
}

/// Turns an Entry's bytes into display text: JSON lines are pretty-printed
/// (when complete and valid), plain text is shown as written.
pub fn format_detail(format: Format, bytes: &[u8], entry_len: usize) -> EntryDetail {
    let truncated = entry_len > bytes.len();
    let raw = String::from_utf8_lossy(bytes);
    let raw = raw.trim_end_matches(['\n', '\r']);
    if format == Format::JsonLines
        && !truncated
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim())
        && let Ok(pretty) = serde_json::to_string_pretty(&value)
    {
        return EntryDetail { text: pretty, size: entry_len, truncated };
    }
    EntryDetail { text: raw.replace("\r\n", "\n"), size: entry_len, truncated }
}

/// Reads one Entry in full (up to [`DETAIL_READ_BYTES`]); `None` when it is not indexed.
pub fn read_detail(index: &EntryIndex, reader: &BlockReader, entry: usize, format: Format) -> io::Result<Option<EntryDetail>> {
    let Some(range) = index.entry_range(entry) else { return Ok(None) };
    let len = (range.end - range.start) as usize;
    let mut buf = vec![0u8; len.min(DETAIL_READ_BYTES)];
    let n = reader.read_at(range.start, &mut buf)?;
    buf.truncate(n);
    Ok(Some(format_detail(format, &buf, len)))
}

/// The first `hh:mm:ss` (with fractional seconds when written) in `text`, as
/// it appears there. Looks only at the start, where an Entry's time sits.
pub fn time_of_day(text: &str) -> Option<&str> {
    let head = &text.as_bytes()[..text.len().min(400)];
    let digit = |i: usize| head.get(i).is_some_and(u8::is_ascii_digit);
    for i in 0..head.len() {
        let at = |j: usize, c: u8| head.get(i + j) == Some(&c);
        let shape = digit(i) && digit(i + 1) && at(2, b':') && digit(i + 3) && digit(i + 4) && at(5, b':') && digit(i + 6) && digit(i + 7);
        // A longer digit run before it means this is the middle of some other number.
        if shape && (i == 0 || !head[i - 1].is_ascii_digit()) {
            let mut end = i + 8;
            if matches!(head.get(end), Some(b'.' | b',')) && digit(end + 1) {
                end += 1;
                while digit(end) {
                    end += 1;
                }
            }
            return text.get(i..end);
        }
    }
    None
}

/// `118 B`, `4.2 KB`, `1.3 MB`.
pub fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}

/// Number of top-level fields of a JSON-lines Entry, or `None` when it is
/// not a complete JSON object or too long to be read whole.
pub fn json_field_count(index: &EntryIndex, reader: &BlockReader, entry: usize) -> io::Result<Option<usize>> {
    let Some(range) = index.entry_range(entry) else { return Ok(None) };
    let len = (range.end - range.start) as usize;
    if len > DETAIL_READ_BYTES {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    let n = reader.read_at(range.start, &mut buf)?;
    buf.truncate(n);
    Ok(match serde_json::from_slice::<serde_json::Value>(&buf) {
        Ok(serde_json::Value::Object(map)) => Some(map.len()),
        _ => None,
    })
}

/// What the Entry view shows for one Entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySummary {
    pub first_line: String,
    /// Continuation lines after the first.
    pub extra_lines: usize,
    /// The Entry was longer than what was examined, so `extra_lines` is a
    /// lower bound.
    pub more: bool,
    /// The first line was cut at [`MAX_DISPLAY_BYTES`].
    pub truncated: bool,
}

pub fn summarize(bytes: &[u8], entry_len: usize) -> EntrySummary {
    let (line, rest) = match memchr::memchr(b'\n', bytes) {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &bytes[bytes.len()..]),
    };
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let truncated = line.len() > MAX_DISPLAY_BYTES;
    let shown = &line[..line.len().min(MAX_DISPLAY_BYTES)];
    let mut first_line = String::from_utf8_lossy(shown).into_owned();
    if truncated {
        first_line.push('…');
    }
    let mut extra_lines = memchr::memchr_iter(b'\n', rest).count();
    if !rest.is_empty() && !rest.ends_with(b"\n") {
        extra_lines += 1;
    }
    EntrySummary { first_line, extra_lines, more: entry_len > bytes.len(), truncated }
}

/// Reads the summary of Entry `i`.
pub fn read_summary(index: &EntryIndex, reader: &BlockReader, i: usize) -> io::Result<Option<EntrySummary>> {
    let Some(range) = index.entry_range(i) else { return Ok(None) };
    let len = (range.end - range.start) as usize;
    let mut buf = vec![0u8; len.min(SUMMARY_READ_BYTES)];
    let n = reader.read_at(range.start, &mut buf)?;
    buf.truncate(n);
    Ok(Some(summarize(&buf, len)))
}

#[derive(Debug)]
pub enum CopyError {
    TooLarge { limit: usize },
    Io(io::Error),
}

impl fmt::Display for CopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CopyError::TooLarge { limit } => write!(f, "selection is larger than {} MiB", limit / 1024 / 1024),
            CopyError::Io(e) => write!(f, "cannot read the selection: {e}"),
        }
    }
}

impl std::error::Error for CopyError {}

/// The raw text of the given Entries in the order given, each ending in a
/// newline. Line endings inside the Entries are kept as they are in the file.
pub fn copy_entries(
    index: &EntryIndex,
    reader: &BlockReader,
    entries: impl IntoIterator<Item = usize>,
    limit: usize,
) -> Result<String, CopyError> {
    let mut out = Vec::new();
    for i in entries {
        let Some(range) = index.entry_range(i) else { continue };
        let len = (range.end - range.start) as usize;
        if out.len() + len + 1 > limit {
            return Err(CopyError::TooLarge { limit });
        }
        let start = out.len();
        out.resize(start + len, 0);
        let n = reader.read_at(range.start, &mut out[start..]).map_err(CopyError::Io)?;
        out.truncate(start + n);
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_time_of_day_is_found_as_written() {
        assert_eq!(time_of_day("2026-10-18 00:00:01.455 ERROR x"), Some("00:00:01.455"));
        assert_eq!(time_of_day("{\n  \"time\": \"2026-10-18T19:50:03Z\"\n}"), Some("19:50:03"));
        assert_eq!(time_of_day("Oct 18 07:05:09 host app: x"), Some("07:05:09"));
        assert_eq!(time_of_day("no time here, took 12:3"), None);
        assert_eq!(time_of_day("123:45:67 is not a time"), None);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(4300), "4.2 KB");
        assert_eq!(human_size(1_363_149), "1.3 MB");
    }

    #[test]
    fn json_lines_are_pretty_printed() {
        let d = format_detail(Format::JsonLines, b"{\"level\":\"error\",\"msg\":\"x\"}\n", 28);
        assert_eq!(d.text, "{\n  \"level\": \"error\",\n  \"msg\": \"x\"\n}");
        assert!(!d.truncated);
    }

    #[test]
    fn plain_text_and_invalid_json_are_shown_as_written() {
        let plain = format_detail(Format::PlainText, b"2026 ERROR failed\r\n    at a\r\n", 31);
        assert_eq!(plain.text, "2026 ERROR failed\n    at a");
        let broken = format_detail(Format::JsonLines, b"{not json\n", 11);
        assert_eq!(broken.text, "{not json");
    }

    #[test]
    fn long_entries_are_flagged_truncated_and_not_reformatted() {
        let d = format_detail(Format::JsonLines, b"{\"a\":1", 1_000_000);
        assert!(d.truncated);
        assert_eq!(d.text, "{\"a\":1");
    }

    #[test]
    fn summary_counts_continuation_lines() {
        let s = summarize(b"2026 ERROR failed\n    at a\n    at b\n", 36);
        assert_eq!(s.first_line, "2026 ERROR failed");
        assert_eq!(s.extra_lines, 2);
        assert!(!s.more && !s.truncated);
        let single = summarize(b"just one line\n", 14);
        assert_eq!((single.extra_lines, single.first_line.as_str()), (0, "just one line"));
        let no_newline = summarize(b"head\n  tail", 11);
        assert_eq!(no_newline.extra_lines, 1);
    }

    #[test]
    fn crlf_is_not_shown() {
        assert_eq!(summarize(b"line\r\nmore\r\n", 12).first_line, "line");
    }

    #[test]
    fn long_first_lines_are_cut() {
        let long = vec![b'x'; MAX_DISPLAY_BYTES + 100];
        let s = summarize(&long, long.len());
        assert!(s.truncated);
        assert!(s.first_line.ends_with('…'));
        assert_eq!(s.first_line.chars().count(), MAX_DISPLAY_BYTES + 1);
    }

    #[test]
    fn partially_read_entries_are_marked() {
        let s = summarize(b"head\nmore\n", 10_000);
        assert!(s.more);
    }

    fn fixture(data: &str) -> (tempfile::NamedTempFile, BlockReader, EntryIndex) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data.as_bytes()).unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let (_, mut index) = EntryIndex::open(&reader, None).unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        index.commit_tail();
        (file, reader, index)
    }

    const LOG: &str = "2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 ERROR b\n    at x\n2026-10-18 19:50:03 INFO c";

    #[test]
    fn summaries_come_from_the_file() {
        let (_f, reader, index) = fixture(LOG);
        let s = read_summary(&index, &reader, 1).unwrap().unwrap();
        assert_eq!(s.first_line, "2026-10-18 19:50:02 ERROR b");
        assert_eq!(s.extra_lines, 1);
        assert!(read_summary(&index, &reader, 9).unwrap().is_none());
    }

    #[test]
    fn json_objects_report_their_field_count() {
        let (_f, reader, index) = fixture("{\"level\":\"info\",\"msg\":\"a\",\"n\":1}\n[1,2]\n");
        assert_eq!(json_field_count(&index, &reader, 0).unwrap(), Some(3));
        assert_eq!(json_field_count(&index, &reader, 1).unwrap(), None);
        assert_eq!(json_field_count(&index, &reader, 9).unwrap(), None);
    }

    #[test]
    fn copy_keeps_order_and_ends_each_entry_with_a_newline() {
        let (_f, reader, index) = fixture(LOG);
        let text = copy_entries(&index, &reader, [2, 1], 1 << 20).unwrap();
        assert_eq!(text, "2026-10-18 19:50:03 INFO c\n2026-10-18 19:50:02 ERROR b\n    at x\n");
    }

    #[test]
    fn copy_refuses_selections_over_the_limit() {
        let (_f, reader, index) = fixture(LOG);
        assert!(matches!(copy_entries(&index, &reader, 0..3, 20), Err(CopyError::TooLarge { .. })));
    }
}
