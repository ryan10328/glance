---
status: accepted
---

# Read Log Files by position with a block cache, an incremental line index and virtual scrolling

Glance targets Log Files of several gigabytes on macOS, Linux and Windows, so it never loads a whole file into memory. Files are read with positional reads (`pread`-style) through a small block cache, an incrementally updated index records Entry boundaries, and the UI renders only the visible range. The index must be updatable in place because Follow Tail appends Entries while a file is open.

We deliberately do not memory-map files. On Linux and macOS, reading a mapped region after the file is truncated or rotated raises SIGBUS and crashes the app; on Windows, a mapped file cannot be renamed or deleted, which could break the logger or logrotate of the service being inspected. Positional reads behave the same on all three platforms: a truncated file yields a short read, and the file is never locked.

v1 supports UTF-8 only, with invalid bytes shown as replacement characters, so the index can find line breaks directly in the raw bytes. Encoding is a per-file property, so detection and manual selection can be added later without redesigning the index.

## Considered Options

- **Load the whole file into memory**: simplest, but unusable for multi-gigabyte files and wasteful with several tabs open.
- **Memory-map with size checks**: fastest full-file scans, but a truncation between the check and the read still crashes, and Windows file locking remains.
- **Memory-map on Linux/macOS, positional reads on Windows**: avoids the Windows lock but keeps the crash risk and needs two read paths.
- **Multi-encoding support in v1**: rejected for now; it would make line indexing encoding-aware for little immediate value.

## Consequences

- We own the block cache (block size, eviction, read-ahead) and accept slower whole-file scans than mmap; large sequential reads (around 1 MB) keep indexing, Filter and Search near disk speed.
- Truncation or rotation can still leave the index out of sync with the file, so detection is required: Glance stops following, prompts the user, and reloads only on request.
- Wrapping long lines is optional and off by default, because variable Entry heights make scroll positions costly to compute on huge files.
- Background tabs keep their index current, but Filter and Search only run in the active tab.
