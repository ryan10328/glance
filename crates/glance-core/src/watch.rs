//! Change detection for an open Log File, by polling (no filesystem events).
//!
//! Distinguishes growth (Follow Tail) from Rotation: the file being replaced,
//! truncated or removed. On Rotation Glance stops following and lets the user
//! decide whether to reload.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::reader::{BlockReader, FileId};

/// Poll interval for the active Tab.
pub const ACTIVE_POLL: Duration = Duration::from_millis(500);
/// Poll interval for background Tabs.
pub const BACKGROUND_POLL: Duration = Duration::from_secs(2);

/// Bytes at the start of the file remembered to notice an in-place rewrite.
const FINGERPRINT_LEN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// The path now points to a different file (rename-and-recreate).
    Replaced,
    /// The same file got shorter or its beginning changed (copy-truncate).
    Truncated,
    /// Nothing exists at the path any more.
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    Unchanged,
    /// New bytes were appended.
    Grew { to: u64 },
    Rotated(Rotation),
}

pub struct FileWatcher {
    path: PathBuf,
    id: FileId,
    head: Vec<u8>,
}

impl FileWatcher {
    pub fn new(path: &Path, reader: &BlockReader) -> io::Result<Self> {
        let mut watcher = Self { path: path.to_owned(), id: reader.file_id()?, head: Vec::new() };
        watcher.remember_head(reader)?;
        Ok(watcher)
    }

    fn remember_head(&mut self, reader: &BlockReader) -> io::Result<()> {
        let mut head = vec![0u8; FINGERPRINT_LEN];
        let n = reader.read_uncached(0, &mut head)?;
        head.truncate(n);
        self.head = head;
        Ok(())
    }

    /// Compares the file with what has been consumed so far (`scanned` bytes).
    pub fn poll(&mut self, reader: &BlockReader, scanned: u64) -> io::Result<FileChange> {
        match fs::metadata(&self.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(FileChange::Rotated(Rotation::Removed));
            }
            Err(e) => return Err(e),
            Ok(meta) if FileId::of(&meta) != self.id => {
                return Ok(FileChange::Rotated(Rotation::Replaced));
            }
            Ok(_) => {}
        }
        let len = reader.len()?;
        if len < scanned {
            return Ok(FileChange::Rotated(Rotation::Truncated));
        }
        let mut now = vec![0u8; self.head.len()];
        let n = reader.read_uncached(0, &mut now)?;
        if n < self.head.len() || now != self.head {
            return Ok(FileChange::Rotated(Rotation::Truncated));
        }
        if len > scanned {
            if self.head.len() < FINGERPRINT_LEN {
                self.remember_head(reader)?; // a short file grew; extend the fingerprint
            }
            return Ok(FileChange::Grew { to: len });
        }
        Ok(FileChange::Unchanged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture {
        dir: tempfile::TempDir,
        path: PathBuf,
        reader: BlockReader,
        watcher: FileWatcher,
    }

    fn fixture(content: &[u8]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, content).unwrap();
        let reader = BlockReader::open(&path).unwrap();
        let watcher = FileWatcher::new(&path, &reader).unwrap();
        Fixture { dir, path, reader, watcher }
    }

    #[test]
    fn unchanged_then_grown() {
        let mut fx = fixture(b"line one\n");
        assert_eq!(fx.watcher.poll(&fx.reader, 9).unwrap(), FileChange::Unchanged);
        let mut f = fs::OpenOptions::new().append(true).open(&fx.path).unwrap();
        f.write_all(b"line two\n").unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 9).unwrap(), FileChange::Grew { to: 18 });
        // Once consumed, it is unchanged again.
        assert_eq!(fx.watcher.poll(&fx.reader, 18).unwrap(), FileChange::Unchanged);
    }

    #[cfg(unix)]
    #[test]
    fn rename_and_recreate_is_replacement() {
        let mut fx = fixture(b"old content\n");
        fs::rename(&fx.path, fx.dir.path().join("app.log.1")).unwrap();
        fs::write(&fx.path, b"new content\n").unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 12).unwrap(), FileChange::Rotated(Rotation::Replaced));
    }

    #[test]
    fn removed_file() {
        let mut fx = fixture(b"x\n");
        fs::remove_file(&fx.path).unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 2).unwrap(), FileChange::Rotated(Rotation::Removed));
    }

    #[test]
    fn truncation_to_a_shorter_length() {
        let mut fx = fixture(b"a long first generation of the log\n");
        let f = fs::OpenOptions::new().write(true).open(&fx.path).unwrap();
        f.set_len(5).unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 35).unwrap(), FileChange::Rotated(Rotation::Truncated));
    }

    #[test]
    fn copy_truncate_rewrite_that_ends_up_longer_is_noticed_by_its_beginning() {
        let mut fx = fixture(b"2026-10-18 first generation\n");
        let mut f = fs::OpenOptions::new().write(true).truncate(true).open(&fx.path).unwrap();
        f.write_all(b"2027-01-01 second generation, written afterwards and longer\n").unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 28).unwrap(), FileChange::Rotated(Rotation::Truncated));
    }

    #[test]
    fn a_file_that_started_empty_can_grow() {
        let mut fx = fixture(b"");
        let mut f = fs::OpenOptions::new().append(true).open(&fx.path).unwrap();
        f.write_all(b"first\n").unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 0).unwrap(), FileChange::Grew { to: 6 });
        f.write_all(b"second\n").unwrap();
        assert_eq!(fx.watcher.poll(&fx.reader, 6).unwrap(), FileChange::Grew { to: 13 });
    }
}
