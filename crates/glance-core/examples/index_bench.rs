//! Generates a synthetic log and times indexing.
//! Usage: cargo run --release -p glance-core --example index_bench -- <dir> [size_mb]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::Instant;

use glance_core::index::{EntryIndex, IndexHandle};
use glance_core::reader::BlockReader;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: index_bench <dir> [size_mb]");
    let size_mb: u64 = args.next().map_or(1024, |s| s.parse().unwrap());
    let path = std::path::Path::new(&dir).join("bench.log");

    let target = size_mb * 1024 * 1024;
    let mut out = BufWriter::new(File::create(&path)?);
    let (mut written, mut n) = (0u64, 0u64);
    while written < target {
        let line = if n % 20 == 3 {
            format!("2026-10-18 19:50:{:02}.{:03} ERROR request {n} failed: timeout\n    at handler.rs:42\n    at router.rs:118\n", n % 60, n % 1000)
        } else {
            format!("2026-10-18 19:50:{:02}.{:03} INFO  request {n} served in {}ms path=/api/v1/items/{}\n", n % 60, n % 1000, n % 97, n % 5003)
        };
        out.write_all(line.as_bytes())?;
        written += line.len() as u64;
        n += 1;
    }
    out.flush()?;
    drop(out);
    println!("generated {} MiB, {n} lines", written / 1024 / 1024);

    let start = Instant::now();
    let reader = Arc::new(BlockReader::open(&path)?);
    let (_format, index) = EntryIndex::open(&reader, None)?;
    let handle = IndexHandle::spawn(reader, index);
    let shared = handle.index().clone();
    let first_page = loop {
        if shared.read().unwrap().len() >= 50 {
            break start.elapsed();
        }
        std::thread::yield_now();
    };
    handle.join()?;
    let total = start.elapsed();
    let index = shared.read().unwrap();
    println!("first 50 entries after {first_page:?}");
    println!("indexed {} entries in {total:?} ({:.0} MiB/s)", index.len(), written as f64 / 1048576.0 / total.as_secs_f64());
    std::fs::remove_file(&path)?;
    Ok(())
}
