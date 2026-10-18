//! Reading Entries for display and for copying.

use std::fmt;
use std::io;

use crate::index::EntryIndex;
use crate::reader::BlockReader;

/// Bytes of a first line shown in the Entry view; the rest is cut with `…`.
pub const MAX_DISPLAY_BYTES: usize = 4096;
/// How much of an Entry is read to summarize it.
const SUMMARY_READ_BYTES: usize = 64 * 1024;

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
