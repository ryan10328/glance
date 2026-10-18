# Glance

A fast desktop log viewer built with Rust and [GPUI](https://www.gpui.rs/) via [gpui-kit](https://github.com/longbridge/gpui-kit).

> **Status:** early development. There is no usable release yet.

## Goals

- Open local log files of several gigabytes without loading them into memory.
- Work with plain-text and JSON-lines logs, detecting the format automatically.
- Search and filter by level and text (wildcard or regex).
- Highlight entries by Level and color any text you choose.
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

## Highlight rules

Besides the built-in Level colors, the **Highlight** row under the Filter and Search bars colors any text you choose, as you type. Pick a color for each rule; `*` and `?` are wildcards unless `.* Regex` is on. Only the matching text changes color, so Level colors stay readable. Rules are shared by every tab and remembered in `state.toml`; where two rules overlap, the upper one wins. Search matches get a background on top.

## License

[MIT](LICENSE.md)
