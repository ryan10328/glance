//! Block reader: positional reads behind a small block cache (see ADR 0001).
//!
//! Files are never memory-mapped. A file that is truncated or rotated while
//! open yields short reads, never a crash, and is never locked.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const DEFAULT_BLOCK_SIZE: usize = 64 * 1024;
pub const DEFAULT_CACHE_BLOCKS: usize = 256;
/// Suggested buffer size for whole-file scans (indexing, Filter, Search).
pub const SCAN_CHUNK_SIZE: usize = 1024 * 1024;

#[cfg(unix)]
fn read_at_raw(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at_raw(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

/// Reads until `buf` is full or the end of the file, retrying on interrupts.
fn read_full_at(file: &File, buf: &mut [u8], mut offset: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match read_at_raw(file, &mut buf[filled..], offset) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[derive(Default)]
struct Cache {
    blocks: HashMap<u64, Arc<[u8]>>,
    /// Least recently used first.
    order: VecDeque<u64>,
}

impl Cache {
    fn get(&mut self, index: u64) -> Option<Arc<[u8]>> {
        let block = self.blocks.get(&index)?.clone();
        if let Some(pos) = self.order.iter().position(|&i| i == index) {
            self.order.remove(pos);
        }
        self.order.push_back(index);
        Some(block)
    }

    fn insert(&mut self, index: u64, block: Arc<[u8]>, capacity: usize) {
        if self.blocks.insert(index, block).is_none() {
            self.order.push_back(index);
        }
        while self.blocks.len() > capacity {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.blocks.remove(&oldest);
                }
                None => break,
            }
        }
    }
}

pub struct BlockReader {
    file: File,
    block_size: usize,
    capacity: usize,
    cache: Mutex<Cache>,
}

impl BlockReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::with_options(File::open(path)?, DEFAULT_BLOCK_SIZE, DEFAULT_CACHE_BLOCKS)
    }

    pub fn with_options(file: File, block_size: usize, cache_blocks: usize) -> io::Result<Self> {
        assert!(block_size > 0 && cache_blocks > 0);
        Ok(Self { file, block_size, capacity: cache_blocks, cache: Mutex::default() })
    }

    /// Current size of the file on disk. It can grow (Follow Tail) or shrink
    /// (truncation), so it is queried each time.
    pub fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Drops every cached block. Call after Rotation or truncation.
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap() = Cache::default();
    }

    /// Reads up to `buf.len()` bytes at `offset` through the block cache.
    /// Returns fewer bytes (possibly 0) at or past the end of the file.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let block_size = self.block_size as u64;
        let mut copied = 0;
        while copied < buf.len() {
            let position = offset + copied as u64;
            let block = self.block(position / block_size)?;
            let within = (position % block_size) as usize;
            if within >= block.len() {
                break;
            }
            let n = (block.len() - within).min(buf.len() - copied);
            buf[copied..copied + n].copy_from_slice(&block[within..within + n]);
            copied += n;
            if block.len() < self.block_size {
                break;
            }
        }
        Ok(copied)
    }

    /// Reads straight from the file, bypassing the cache. Intended for large
    /// sequential scans (use buffers of about [`SCAN_CHUNK_SIZE`]).
    pub fn read_uncached(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        read_full_at(&self.file, buf, offset)
    }

    fn block(&self, index: u64) -> io::Result<Arc<[u8]>> {
        if let Some(hit) = self.cache.lock().unwrap().get(index) {
            return Ok(hit);
        }
        let mut data = vec![0u8; self.block_size];
        let n = read_full_at(&self.file, &mut data, index * self.block_size as u64)?;
        data.truncate(n);
        let block: Arc<[u8]> = data.into();
        // Only full blocks are cached: a partial last block goes stale as the
        // file grows.
        if n == self.block_size {
            self.cache.lock().unwrap().insert(index, block.clone(), self.capacity);
        }
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn reader_over(data: &[u8], block_size: usize, cache_blocks: usize) -> (tempfile::NamedTempFile, BlockReader) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
        let reader = BlockReader::with_options(File::open(file.path()).unwrap(), block_size, cache_blocks).unwrap();
        (file, reader)
    }

    #[test]
    fn reads_across_block_boundaries() {
        let data = pattern(1000);
        let (_f, reader) = reader_over(&data, 64, 4);
        let mut buf = vec![0u8; 300];
        assert_eq!(reader.read_at(50, &mut buf).unwrap(), 300);
        assert_eq!(buf, data[50..350]);
    }

    #[test]
    fn short_read_at_end_of_file() {
        let data = pattern(100);
        let (_f, reader) = reader_over(&data, 64, 4);
        let mut buf = vec![0u8; 50];
        assert_eq!(reader.read_at(80, &mut buf).unwrap(), 20);
        assert_eq!(buf[..20], data[80..]);
        assert_eq!(reader.read_at(100, &mut buf).unwrap(), 0);
        assert_eq!(reader.read_at(5000, &mut buf).unwrap(), 0);
    }

    #[test]
    fn crlf_bytes_are_preserved() {
        let data = b"one\r\ntwo\r\nthree\r\n".to_vec();
        let (_f, reader) = reader_over(&data, 4, 4);
        let mut buf = vec![0u8; data.len()];
        assert_eq!(reader.read_at(0, &mut buf).unwrap(), data.len());
        assert_eq!(buf, data);
    }

    #[test]
    fn truncation_yields_short_reads_not_a_crash() {
        let data = pattern(500);
        let (f, reader) = reader_over(&data, 64, 2);
        let mut buf = vec![0u8; 200];
        assert_eq!(reader.read_uncached(300, &mut buf).unwrap(), 200);

        f.as_file().set_len(100).unwrap();
        assert_eq!(reader.len().unwrap(), 100);
        reader.invalidate();
        assert_eq!(reader.read_at(300, &mut buf).unwrap(), 0);
        assert_eq!(reader.read_uncached(300, &mut buf).unwrap(), 0);
        assert_eq!(reader.read_at(60, &mut buf).unwrap(), 40);
    }

    #[test]
    fn growth_is_visible_through_a_partial_last_block() {
        let (mut f, reader) = reader_over(b"hello", 64, 4);
        let mut buf = [0u8; 16];
        assert_eq!(reader.read_at(0, &mut buf).unwrap(), 5);

        f.write_all(b" world").unwrap();
        f.flush().unwrap();
        assert_eq!(reader.read_at(0, &mut buf).unwrap(), 11);
        assert_eq!(&buf[..11], b"hello world");
    }

    #[test]
    fn cache_evicts_least_recently_used_blocks() {
        let data = pattern(64 * 10);
        let (_f, reader) = reader_over(&data, 64, 3);
        let mut buf = [0u8; 1];
        for block in 0..10u64 {
            reader.read_at(block * 64, &mut buf).unwrap();
        }
        let cache = reader.cache.lock().unwrap();
        assert_eq!(cache.blocks.len(), 3);
        assert!(cache.blocks.contains_key(&9) && cache.blocks.contains_key(&7));
    }

    #[test]
    fn invalidate_clears_the_cache() {
        let data = pattern(256);
        let (_f, reader) = reader_over(&data, 64, 4);
        let mut buf = [0u8; 64];
        reader.read_at(0, &mut buf).unwrap();
        assert_eq!(reader.cache.lock().unwrap().blocks.len(), 1);
        reader.invalidate();
        assert!(reader.cache.lock().unwrap().blocks.is_empty());
    }

    #[test]
    fn empty_file() {
        let (_f, reader) = reader_over(b"", 64, 4);
        assert!(reader.is_empty().unwrap());
        let mut buf = [0u8; 8];
        assert_eq!(reader.read_at(0, &mut buf).unwrap(), 0);
    }
}
