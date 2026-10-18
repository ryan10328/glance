# v1 Backlog

Ordered by dependency and complexity, simplest first. Terms follow `GLOSSARY.md`; architecture follows `docs/adr/0001-large-file-architecture.md`. "core" is `glance-core` (no GPUI dependency); "ui" is `glance`.

## Out of scope

Multi-file merged timeline, OS file association, saving Filter results to a file, multiple text encodings, custom Format rules (regex), column/table view of Entries, CI and packaging.

## Progress

- [x] 1. Settings paths and files
- [x] 2. Level normalization
- [x] 3. Block reader
- [x] 4. Incremental Entry index
- [x] 5. Format Detection and parsers
- [x] 6. Filter engine
- [x] 7. Search engine
- [x] 8. Rotation detection and Follow Tail data path
- [x] 9. Window shell and Tabs
- [x] 10. Virtualized Entry view
- [x] 11. Filter and Search UI
- [x] 12a. Level coloring and Search occurrence highlighting
- [x] 12b. Highlight Rules in the window (text color, remembered)
- [x] 12d. Color theme (System, Light, Dark) from the Tab bar
- [x] 12c. Settings window
- [x] 13. Follow Tail UI
- [ ] 14. Detail panel
- [ ] 15. Keyboard shortcuts
- [ ] 16. Bookmarks (low priority)

## Tickets

### 1. Settings paths and files (core)
- Resolve the config directory with `directories::ProjectDirs::from("com", "ryan", "Glance")`; debug builds append `-dev`.
- Load `config.toml` (user-edited preferences) and `state.toml` (program-written: window size, recent files, Highlight Rules, color theme) via `serde`; missing files fall back to defaults.
- Tests: path resolution, defaults, malformed TOML does not crash.

### 2. Level normalization (core)
- Map raw strings and aliases (`warning`, `err`, `critical`, ...) to TRACE..FATAL; unknown means no Level.
- Tests: alias table, case-insensitivity, unknown values.

### 3. Block reader (core)
- Positional reads (`pread`-style) behind a small block cache; large sequential reads (~1 MB) for scans. No memory mapping.
- Truncation yields a short read, never a crash.
- Tests: cross-block reads, truncation during read, files with `\r\n`.

### 4. Incremental Entry index (core)
- Background indexing from the block reader; first page available immediately, progress reported.
- Plain-text Entries start at a timestamped line; files without timestamps fall back to one line per Entry.
- Index can be extended in place when the file grows (needed by Follow Tail).
- Tests and a benchmark on a generated 1 GB file (first page under 1 s).

### 5. Format Detection and parsers (core)
- Plain text and JSON lines first; detection samples the start of the file; manual override per Log File.
- Parsed output: timestamp, Level, message, remaining fields.
- Time zone: timestamps without a zone use the local zone; displayed text is never altered. No time zone setting, since Glance has no time-based features.

### 6. Filter engine (core)
- Conditions: Level (multi-select, "WARN and above", separate "no Level" option, default on), text (wildcard or regex, case-sensitivity toggle); combined with AND.
- Context: keep N Entries around each match.
- Runs over the indexed range only and updates as indexing progresses.

### 7. Search engine (core)
- Text/regex, case-sensitivity option, next/previous match, match count; independent of Filter and usable together with it.

### 8. Rotation detection and Follow Tail data path (core)
- Detect growth, rotation and truncation of the open Log File.
- On Rotation: stop following and report; reload only when requested.

### 9. Window shell and Tabs (ui)
- Layout: Tab bar, Filter bar, Highlight row, Search bar (always shown; Cmd/Ctrl+F focuses it), Entry view, collapsible detail panel, footer. The footer is the only place showing entry counts, with a progress bar on the right while indexing or filtering.
- Open files via dialog, drag and drop, and a command-line argument.
- Per-Tab state; background Tabs keep their index current, Filter and Search run only in the active Tab.

### 10. Virtualized Entry view (ui)
- Render only visible rows at fixed row height; wrapping is a toggle, off by default.
- Entry selection (click, Shift range, Cmd/Ctrl add) and copy of full Entries including continuation lines.

### 11. Filter and Search UI (ui)
- Filter bar: Level checkboxes (with All and "WARN and above" presets), text conditions (wildcard or regex, case-sensitivity, exclude). Context N is the `filter_context` setting in `config.toml`, not a bar field. Field and time conditions were dropped by design.
- Search bar: live search, next/previous, count, case-sensitivity and regex toggles, and "Find all" (Cmd/Ctrl+Enter) that lists every match in a resizable results panel below the Entry view (a frozen list; click a result to select it and scroll to it).
- "Show Context" for a single Entry and jump to next ERROR-or-above moved to tickets 14 and 15.

### 12. Highlighting (ui + core), in three steps
- 12a: built-in Level rules coloring the whole Entry; mark Search occurrences in the visible first line.
- 12b: Highlight Rules typed in the window, applied live as text color on the matching text; shared by all Tabs, remembered in `state.toml`, first rule wins.
- 12c: Settings window (`⚙` in the Tab bar, Cmd/Ctrl+,) with theme, Entry view font (installed fonts, searchable) and size with a live preview, and Filter context; changes apply at once and are saved to `config.toml` (which drops comments in that file); direct file edits are read when the app starts.
- 12d: color theme choice (System, Light, Dark) in the Tab bar, remembered in `state.toml`; 12c's settings window offers the same choice. Highlight palette colors have light and dark variants.

### 13. Follow Tail UI (ui)
- Toggle in the footer (off by default, per Tab, not remembered); auto-pause when scrolling up, resume at the bottom or with the floating "Resume Follow" button.
- Rotation notice bar with a "Reload" button (built in ticket 9); a Rotation also turns Follow off.

### 14. Detail panel (ui)
- Selected Entry in full: pretty-printed JSON for JSON lines, full text otherwise.
- Copy a field value.
- "Show Context" for a single Entry (moved from ticket 11): show the Entries around the selected one without a Filter.
- Mark Search occurrences that fall outside the first line (the Entry view shows only the first line).

### 15. Keyboard shortcuts (ui)
- Jump to the next ERROR-or-above Entry (moved from ticket 11).
- Desktop conventions with Cmd on macOS and Ctrl elsewhere; `less`-style single keys (`/`, `n`, `N`, `G`) when no input has focus.

### 16. Bookmarks (low priority)
- Mark an Entry, jump between Bookmarks. Persistence per file to be decided when this is built.
