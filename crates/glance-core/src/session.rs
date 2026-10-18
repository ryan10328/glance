//! An open Log File: reader, index, change detection and the Follow Tail
//! data path.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::format::Format;
use crate::index::{EntryIndex, IndexHandle};
use crate::reader::BlockReader;
use crate::watch::{FileChange, FileWatcher, Rotation};

/// Bytes consumed per lock hold while catching up with a growing file.
const FOLLOW_STEP: u64 = 8 * 1024 * 1024;

pub struct LogFile {
    path: PathBuf,
    reader: Arc<BlockReader>,
    index: Arc<RwLock<EntryIndex>>,
    handle: IndexHandle,
    watcher: FileWatcher,
    format: Format,
    format_override: Option<Format>,
    rotated: Option<Rotation>,
    generation: u64,
}

/// What a [`LogFile::poll`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polled {
    Unchanged,
    /// New Entries may be available; update Filter and Search results.
    Grew,
    /// The file was rotated; following has stopped until [`LogFile::reload`].
    Rotated(Rotation),
}

impl LogFile {
    /// Opens `path` and starts indexing in the background. `format` is the
    /// user's choice, or `None` to detect it.
    pub fn open(path: &Path, format: Option<Format>) -> io::Result<Self> {
        let reader = Arc::new(BlockReader::open(path)?);
        let (detected, index) = EntryIndex::open(&reader, format)?;
        let watcher = FileWatcher::new(path, &reader)?;
        let handle = IndexHandle::spawn(reader.clone(), index);
        Ok(Self {
            path: path.to_owned(),
            index: handle.index().clone(),
            reader,
            handle,
            watcher,
            format: detected,
            format_override: format,
            rotated: None,
            generation: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reader(&self) -> &Arc<BlockReader> {
        &self.reader
    }

    pub fn index(&self) -> &Arc<RwLock<EntryIndex>> {
        &self.index
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Set once Rotation is detected; cleared by [`Self::reload`].
    pub fn rotated(&self) -> Option<Rotation> {
        self.rotated
    }

    /// Increases on every [`Self::reload`]; Filter and Search results built
    /// for an earlier generation must be discarded.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the first indexing pass over the file has finished.
    pub fn is_indexed(&self) -> bool {
        self.handle.is_finished()
    }

    /// Checks the file and, when it grew, extends the index. Call every
    /// [`crate::watch::ACTIVE_POLL`] (or `BACKGROUND_POLL` for background Tabs).
    /// After Rotation it reports the same state without touching the file.
    pub fn poll(&mut self) -> io::Result<Polled> {
        if let Some(rotation) = self.rotated {
            return Ok(Polled::Rotated(rotation));
        }
        let scanned = self.index.read().unwrap().scanned();
        match self.watcher.poll(&self.reader, scanned)? {
            FileChange::Unchanged => Ok(Polled::Unchanged),
            FileChange::Rotated(rotation) => {
                self.rotated = Some(rotation);
                Ok(Polled::Rotated(rotation))
            }
            FileChange::Grew { .. } => {
                // While the first pass is running it picks up the growth itself.
                if self.handle.is_finished() {
                    loop {
                        let progress = self.index.write().unwrap().advance(&self.reader, FOLLOW_STEP)?;
                        if progress.is_complete() {
                            break;
                        }
                    }
                    self.index.write().unwrap().commit_tail();
                }
                Ok(Polled::Grew)
            }
        }
    }

    /// Reopens the path from scratch (after Rotation, or on request). Keeps
    /// the user's Format choice. Callers must rebuild their views.
    pub fn reload(&mut self) -> io::Result<()> {
        let generation = self.generation + 1;
        *self = Self { generation, ..Self::open(&self.path, self.format_override)? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::time::{Duration, Instant};

    fn wait_indexed(file: &LogFile) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !file.is_indexed() {
            assert!(Instant::now() < deadline, "indexing did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn entries(file: &LogFile) -> usize {
        file.index().read().unwrap().len()
    }

    #[test]
    fn following_picks_up_appended_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "2026-10-18 19:50:01 INFO a\n2026-10-18 19:50:02 INFO b\n").unwrap();
        let mut file = LogFile::open(&path, None).unwrap();
        wait_indexed(&file);
        assert_eq!(file.poll().unwrap(), Polled::Unchanged);
        assert_eq!(entries(&file), 2);

        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"2026-10-18 19:50:03 INFO c\n    detail\n").unwrap();
        assert_eq!(file.poll().unwrap(), Polled::Grew);
        assert_eq!(entries(&file), 3);

        // A line written in two pieces is not split into two Entries.
        f.write_all(b"2026-10-18 19:50:04 IN").unwrap();
        file.poll().unwrap();
        f.write_all(b"FO d\n").unwrap();
        file.poll().unwrap();
        assert_eq!(entries(&file), 4);
        assert_eq!(file.poll().unwrap(), Polled::Unchanged);
    }

    #[test]
    fn rotation_stops_following_until_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "2026-10-18 19:50:01 INFO old\n").unwrap();
        let mut file = LogFile::open(&path, None).unwrap();
        wait_indexed(&file);

        fs::write(&path, "2027-01-01 00:00:01 INFO brand new generation of the log\n").unwrap();
        assert!(matches!(file.poll().unwrap(), Polled::Rotated(_)));
        assert!(file.rotated().is_some());
        assert_eq!(entries(&file), 1);
        // Still reported, and nothing is read, until the user reloads.
        assert!(matches!(file.poll().unwrap(), Polled::Rotated(_)));

        file.reload().unwrap();
        assert_eq!(file.rotated(), None);
        assert_eq!(file.generation(), 1);
        wait_indexed(&file);
        assert_eq!(entries(&file), 1);
        assert_eq!(file.poll().unwrap(), Polled::Unchanged);
    }

    #[test]
    fn reload_keeps_the_users_format_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let mut file = LogFile::open(&path, Some(Format::PlainText)).unwrap();
        assert_eq!(file.format(), Format::PlainText);
        file.reload().unwrap();
        assert_eq!(file.format(), Format::PlainText);
    }

    #[test]
    fn opening_a_binary_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        fs::write(&path, b"\0\0\0binary").unwrap();
        assert!(LogFile::open(&path, None).is_err());
    }

    #[test]
    fn a_removed_file_is_reported_as_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, "one\n").unwrap();
        let mut file = LogFile::open(&path, None).unwrap();
        wait_indexed(&file);
        fs::remove_file(&path).unwrap();
        assert_eq!(file.poll().unwrap(), Polled::Rotated(Rotation::Removed));
        assert!(file.reload().is_err()); // nothing to reopen yet
    }
}
