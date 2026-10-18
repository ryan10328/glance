//! Incremental Entry index (see ADR 0001).
//!
//! The index records where each Entry starts. It is built in bounded steps
//! ([`EntryIndex::advance`]) so the first page is available immediately, and it
//! extends in place when the file grows (Follow Tail).

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use crate::format::Format;
use crate::reader::{BlockReader, SCAN_CHUNK_SIZE};
use crate::timestamp::{PREFIX_LEN, TimestampStyle};

/// How many leading bytes are sampled for binary and timestamp detection.
pub const SAMPLE_LEN: usize = 64 * 1024;
const SAMPLE_LINES: usize = 200;

/// How lines group into Entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRule {
    /// Every line is an Entry (JSON lines, or text without timestamps).
    PerLine,
    /// A line opening with a timestamp starts an Entry; other lines continue
    /// it. Lines before the first timestamp form one preamble Entry.
    Timestamped(TimestampStyle),
}

impl EntryRule {
    /// Chooses a rule from the start of a file; falls back to one line per Entry.
    pub fn detect(sample: &[u8]) -> EntryRule {
        let lines = sample.split(|&b| b == b'\n').take(SAMPLE_LINES).filter(|l| !l.is_empty());
        match TimestampStyle::detect(lines) {
            Some(style) => EntryRule::Timestamped(style),
            None => EntryRule::PerLine,
        }
    }
}

/// Whether `sample` looks like binary data rather than text (it contains NUL).
pub fn looks_binary(sample: &[u8]) -> bool {
    sample.contains(&0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes indexed so far.
    pub scanned: u64,
    /// File size at the last step.
    pub total: u64,
}

impl Progress {
    pub fn is_complete(&self) -> bool {
        self.scanned >= self.total
    }
}

#[derive(Debug)]
pub struct EntryIndex {
    rule: EntryRule,
    /// Start offset of each Entry.
    starts: Vec<u64>,
    /// Offset up to which bytes have been consumed.
    scanned: u64,
    /// Start of the line being consumed (the unterminated tail).
    line_start: u64,
    /// First bytes of the tail line, enough to decide whether it starts an Entry.
    prefix: Vec<u8>,
    /// The tail was committed provisionally as an Entry start and must be
    /// reconsidered when more data arrives.
    provisional: bool,
}

impl EntryIndex {
    pub fn new(rule: EntryRule) -> Self {
        Self {
            rule,
            starts: Vec::new(),
            scanned: 0,
            line_start: 0,
            prefix: Vec::new(),
            provisional: false,
        }
    }

    /// Opens a file: rejects binary data, then detects its Format (unless the
    /// user chose one) and the matching Entry rule from the start of the file.
    pub fn open(reader: &BlockReader, format: Option<Format>) -> io::Result<(Format, Self)> {
        let mut sample = vec![0u8; SAMPLE_LEN];
        let n = reader.read_uncached(0, &mut sample)?;
        sample.truncate(n);
        if looks_binary(&sample) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "file looks binary, not a log"));
        }
        let format = format.unwrap_or_else(|| Format::detect(&sample, n == SAMPLE_LEN));
        Ok((format, Self::new(format.entry_rule(&sample))))
    }

    pub fn rule(&self) -> EntryRule {
        self.rule
    }

    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    pub fn scanned(&self) -> u64 {
        self.scanned
    }

    /// Byte range of Entry `i`, including its trailing newline.
    pub fn entry_range(&self, i: usize) -> Option<Range<u64>> {
        let start = *self.starts.get(i)?;
        let end = self.starts.get(i + 1).copied().unwrap_or(self.scanned);
        Some(start..end)
    }

    /// Index of the Entry containing byte `offset`.
    pub fn entry_at(&self, offset: u64) -> Option<usize> {
        if offset >= self.scanned {
            return None;
        }
        match self.starts.binary_search(&offset) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        }
    }

    /// Consumes up to about `budget` bytes of new data and returns progress.
    /// Only newline-terminated lines are decided; see [`Self::commit_tail`].
    pub fn advance(&mut self, reader: &BlockReader, budget: u64) -> io::Result<Progress> {
        self.rollback_provisional();
        let total = reader.len()?;
        let mut buf = vec![0u8; SCAN_CHUNK_SIZE];
        let stop = self.scanned.saturating_add(budget).min(total);
        while self.scanned < stop {
            let want = ((stop - self.scanned) as usize).min(buf.len());
            let n = reader.read_uncached(self.scanned, &mut buf[..want])?;
            if n == 0 {
                break; // file shrank under us; the caller detects Rotation
            }
            self.consume(&buf[..n]);
        }
        Ok(Progress { scanned: self.scanned, total })
    }

    fn consume(&mut self, chunk: &[u8]) {
        let base = self.scanned;
        let mut pos = 0;
        while pos < chunk.len() {
            let (segment, ends_line) = match memchr::memchr(b'\n', &chunk[pos..]) {
                Some(i) => (&chunk[pos..pos + i], true),
                None => (&chunk[pos..], false),
            };
            let room = PREFIX_LEN.saturating_sub(self.prefix.len());
            self.prefix.extend_from_slice(&segment[..segment.len().min(room)]);
            pos += segment.len();
            if ends_line {
                pos += 1;
                self.finish_line();
                self.line_start = base + pos as u64;
            }
        }
        self.scanned = base + chunk.len() as u64;
    }

    fn finish_line(&mut self) {
        if self.starts_entry() {
            self.starts.push(self.line_start);
        }
        self.prefix.clear();
    }

    fn starts_entry(&self) -> bool {
        self.starts.is_empty()
            || match self.rule {
                EntryRule::PerLine => true,
                EntryRule::Timestamped(style) => style.matches(&self.prefix),
            }
    }

    /// Decides the unterminated tail line now, as if it were complete, so a
    /// file whose last line lacks a newline still shows it. The decision is
    /// undone by the next [`Self::advance`] if the file grows.
    pub fn commit_tail(&mut self) {
        if self.provisional || self.scanned == self.line_start {
            return;
        }
        if self.starts_entry() {
            self.starts.push(self.line_start);
            self.provisional = true;
        }
    }

    fn rollback_provisional(&mut self) {
        if self.provisional {
            self.starts.pop();
            self.provisional = false;
        }
    }
}

/// An index being built on a background thread.
pub struct IndexHandle {
    index: Arc<RwLock<EntryIndex>>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl IndexHandle {
    /// Starts indexing `reader` from the beginning. The index is readable while
    /// the thread works; it stops at the end of the file.
    pub fn spawn(reader: Arc<BlockReader>, index: EntryIndex) -> Self {
        const STEP: u64 = 8 * 1024 * 1024;
        let index = Arc::new(RwLock::new(index));
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = {
            let (index, cancel) = (index.clone(), cancel.clone());
            std::thread::spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let progress = index.write().unwrap().advance(&reader, STEP)?;
                    if progress.is_complete() {
                        index.write().unwrap().commit_tail();
                        break;
                    }
                    // std's RwLock favors writers; without a pause here a
                    // reader (the UI) could starve until indexing finishes.
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                Ok(())
            })
        };
        Self { index, cancel, thread: Some(thread) }
    }

    pub fn index(&self) -> &Arc<RwLock<EntryIndex>> {
        &self.index
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the thread and returns its result.
    pub fn join(mut self) -> io::Result<()> {
        self.thread.take().map_or(Ok(()), |t| t.join().expect("indexing thread panicked"))
    }
}

impl Drop for IndexHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build(data: &[u8], rule: Option<EntryRule>) -> (EntryIndex, tempfile::NamedTempFile, BlockReader) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let mut index = match rule {
            Some(rule) => EntryIndex::new(rule),
            None => EntryIndex::open(&reader, None).unwrap().1,
        };
        index.advance(&reader, u64::MAX).unwrap();
        index.commit_tail();
        (index, file, reader)
    }

    fn texts(index: &EntryIndex, reader: &BlockReader) -> Vec<String> {
        (0..index.len())
            .map(|i| {
                let r = index.entry_range(i).unwrap();
                let mut buf = vec![0u8; (r.end - r.start) as usize];
                reader.read_uncached(r.start, &mut buf).unwrap();
                String::from_utf8(buf).unwrap()
            })
            .collect()
    }

    const TRACE: &str = "2026-10-18 19:50:07 ERROR failed\n    at a.rs:1\n    at b.rs:2\n2026-10-18 19:50:09 INFO ok\n";

    #[test]
    fn stack_trace_stays_in_one_entry() {
        let (index, _f, reader) = build(TRACE.as_bytes(), None);
        assert!(matches!(index.rule(), EntryRule::Timestamped(TimestampStyle::Iso8601)));
        let entries = texts(&index, &reader);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].ends_with("at b.rs:2\n"));
        assert_eq!(entries[1], "2026-10-18 19:50:09 INFO ok\n");
    }

    #[test]
    fn lines_before_first_timestamp_form_one_preamble_entry() {
        let data = "=== app v1 ===\nstarting\n2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 INFO b\n";
        let (index, _f, reader) = build(data.as_bytes(), None);
        let entries = texts(&index, &reader);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], "=== app v1 ===\nstarting\n");
    }

    #[test]
    fn text_without_timestamps_is_one_entry_per_line() {
        let (index, _f, _r) = build(b"one\ntwo\n\nthree\n", None);
        assert_eq!(index.rule(), EntryRule::PerLine);
        assert_eq!(index.len(), 4);
    }

    #[test]
    fn json_lines_are_one_entry_per_line() {
        let data = "{\"level\":\"info\",\"msg\":\"a\"}\n{\"level\":\"error\",\"msg\":\"b\"}\n";
        let (index, _f, _r) = build(data.as_bytes(), None);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn crlf_lines_index_the_same() {
        let (index, _f, _r) = build(TRACE.replace('\n', "\r\n").as_bytes(), None);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn unterminated_last_line_is_committed_and_rolled_back_on_growth() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 IN").unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let mut index = EntryIndex::new(EntryRule::Timestamped(TimestampStyle::Iso8601));
        index.advance(&reader, u64::MAX).unwrap();
        assert_eq!(index.len(), 1);
        index.commit_tail();
        assert_eq!(index.len(), 2);

        file.write_all(b"FO b\n    continuation\n2026-10-18 19:50:03 INFO c\n").unwrap();
        file.flush().unwrap();
        index.advance(&reader, u64::MAX).unwrap();
        index.commit_tail();
        let entries = texts(&index, &reader);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1], "2026-10-18 19:50:02 INFO b\n    continuation\n");
    }

    #[test]
    fn incremental_steps_match_a_single_pass() {
        let data = TRACE.repeat(500);
        let (whole, _f, reader) = build(data.as_bytes(), None);
        let mut stepped = EntryIndex::open(&reader, None).unwrap().1;
        while !stepped.advance(&reader, 777).unwrap().is_complete() {}
        stepped.commit_tail();
        assert_eq!(stepped.starts, whole.starts);
    }

    #[test]
    fn a_line_longer_than_a_chunk_is_handled() {
        let mut data = String::from("2026-10-18 19:50:01 INFO ");
        data.push_str(&"x".repeat(SCAN_CHUNK_SIZE * 2 + 17));
        data.push_str("\n2026-10-18 19:50:02 INFO b\n");
        let (index, _f, _r) = build(data.as_bytes(), None);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn binary_files_are_rejected() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"\x7fELF\0\0\0binary").unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        let err = EntryIndex::open(&reader, None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn format_can_be_overridden_by_the_user() {
        let data = "{\"a\":1}\n{\"a\":2}\n";
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data.as_bytes()).unwrap();
        file.flush().unwrap();
        let reader = BlockReader::open(file.path()).unwrap();
        assert_eq!(EntryIndex::open(&reader, None).unwrap().0, Format::JsonLines);
        let (format, index) = EntryIndex::open(&reader, Some(Format::PlainText)).unwrap();
        assert_eq!(format, Format::PlainText);
        assert_eq!(index.rule(), EntryRule::PerLine);
    }

    #[test]
    fn empty_file_has_no_entries() {
        let (index, _f, _r) = build(b"", None);
        assert!(index.is_empty());
        assert_eq!(index.entry_at(0), None);
    }

    #[test]
    fn entry_at_finds_the_containing_entry() {
        let (index, _f, _r) = build(TRACE.as_bytes(), None);
        let second = index.entry_range(1).unwrap().start;
        assert_eq!(index.entry_at(0), Some(0));
        assert_eq!(index.entry_at(second - 1), Some(0));
        assert_eq!(index.entry_at(second), Some(1));
        assert_eq!(index.entry_at(index.scanned()), None);
    }

    #[test]
    fn background_indexing_finishes_and_is_readable() {
        let data = TRACE.repeat(20_000);
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data.as_bytes()).unwrap();
        file.flush().unwrap();
        let reader = Arc::new(BlockReader::open(file.path()).unwrap());
        let index = EntryIndex::open(&reader, None).unwrap().1;
        let handle = IndexHandle::spawn(reader, index);
        let shared = handle.index().clone();
        handle.join().unwrap();
        assert_eq!(shared.read().unwrap().len(), 40_000);
    }
}
