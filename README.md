# Glance

A fast desktop log viewer built with Rust and [GPUI](https://www.gpui.rs/) via [gpui-kit](https://github.com/longbridge/gpui-kit).

> **Status:** early development. There is no usable release yet.

> **Built entirely with AI.** Every line of code and documentation in this project was written by an AI assistant ([Claude Code](https://claude.com/claude-code)). The human maintainer only describes what is wanted and checks that the features work; the code itself has not been reviewed line by line.

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

Besides the built-in Level colors, the **Highlight** row under the Filter bar colors any text you choose, as you type. Pick a color for each rule; `*` and `?` are wildcards unless `.* Regex` is on. Only the matching text changes color, so Level colors stay readable. Rules are shared by every tab and remembered in `state.toml`; where two rules overlap, the upper one wins. Search matches get a background on top.

## Follow Tail

The footer's **Follow** button keeps the view at the end as new Entries arrive, like `tail -f`. Scrolling up pauses it; scroll back to the bottom or press **Resume Follow** to continue. It is off by default and applies to the current tab only.

## Search

The Search bar sits under the Highlight row (`Cmd/Ctrl+F` jumps to it; `Esc` clears the query): matches are marked live and Enter / Shift+Enter step through them. **Find all** (`Cmd/Ctrl+Enter`) lists every match in a panel below, like Notepad++; click a result to jump to that Entry. The list is a snapshot, so press Find all again after changing the query.

## Settings

Two TOML files live in the per-user settings folder (debug builds use a separate `Glance-dev` folder):

- `config.toml`, edited by you: `wrap_long_lines`, `font_family`, `font_size`, `default_time_zone` and `filter_context` (entries kept around each Filter match, 0 to 50; read when a file is opened). A settings window is planned.
- `state.toml`, written by Glance: window size, recent files, Highlight rules and the color theme (System, Light or Dark, chosen from the Tab bar).

The footer shows the entry count (`42 of 1000 entries` while a Filter is applied) and a progress bar while the file is indexed or filtered.

## Contributing

Contributions are welcome, whether they are bug reports, ideas, documentation or code. Pull requests written with or without AI help are both fine. Please:

- check [`docs/backlog.md`](docs/backlog.md) and [`GLOSSARY.md`](GLOSSARY.md) first, and use the glossary's terms;
- keep `cargo clippy --workspace --all-targets` and `cargo test --workspace` clean;
- use [Conventional Commits](https://www.conventionalcommits.org/) in English for commit messages.

## License

[MIT](LICENSE.md)
