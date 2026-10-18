# Glance

A fast desktop log viewer built with Rust and [GPUI](https://www.gpui.rs/) via [gpui-kit](https://github.com/longbridge/gpui-kit).

> **Status:** early development. There is no usable release yet.

## Goals

- Open local log files of several gigabytes without loading them into memory.
- Work with plain-text and JSON-lines logs, detecting the format automatically.
- Search and filter by level, time range, text or regex, and structured fields.
- Highlight entries with built-in and user-defined rules.
- Follow a file as it grows, like `tail -f`.
- Run on macOS, Linux and Windows.

See [`docs/backlog.md`](docs/backlog.md) for the v1 plan, [`GLOSSARY.md`](GLOSSARY.md) for the project vocabulary, and [`docs/adr/`](docs/adr/) for architecture decisions.

## Building

Requires a recent stable Rust toolchain.

```sh
cargo build
```

The workspace has two crates:

- `crates/glance-core`: file reading, indexing, format parsing, filtering and search. No GPUI dependency.
- `crates/glance`: the GPUI application.

## License

[MIT](LICENSE.md)
