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
- [x] 14a. Detail panel: full Entry, Copy entry, Search marks
- [ ] 18. UI polish from the design critique (planned before 14b; order inside it is decided one by one)
- [ ] 14b. Detail panel: copy a field value, Show Context
- [ ] 15. Keyboard shortcuts
- [ ] 16. Bookmarks (low priority)
- [ ] 17. Third-party license notices (before the first binary release)

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

### 14. Detail panel (ui), in two steps
- 14a: a panel to the right of the Entry view (resizable, about 360 px) that shows the selected Entry in full: pretty-printed JSON for JSON lines, all lines otherwise. It appears while something is selected and can be closed with `×` until the selection changes. Search occurrences and Highlight Rules are shown in every line (the Entry view marks only the first). With several Entries selected it shows the last clicked one and "N selected". "Copy entry" copies the selection. Long lines wrap by default (`wrap_long_lines`, also a checkbox in Settings); with wrapping off the body has scrollbars. The list marks Entries with more to see (`⋯ +N lines`, and `{ } N fields` for a JSON object too long for its row). "Copy entry" answers with "✓ Copied" for a moment.
- 14b: copy a single field value of a JSON Entry (a `Copy` button on the hovered line); "Show Context" for a single Entry (moved from ticket 11): the Entries around the selected one, without a Filter.

### 15. Keyboard shortcuts (ui)
- Jump to the next ERROR-or-above Entry (moved from ticket 11).
- Desktop conventions with Cmd on macOS and Ctrl elsewhere; `less`-style single keys (`/`, `n`, `N`, `G`) when no input has focus.

### 16. Bookmarks (low priority)
- Mark an Entry, jump between Bookmarks. Persistence per file to be decided when this is built.

### 17. Third-party license notices (before the first binary release)
- Source-only releases need nothing; a binary release must ship the licenses of what it contains. gpui-kit and GPUI are Apache-2.0, its bundled icons come from Lucide (ISC, with a Feather MIT notice), and a few dependencies are MPL-2.0 (`dwrote`, `option-ext`; unmodified, so only the source has to stay obtainable).
- Generate `THIRD-PARTY-NOTICES` with `cargo-about` and ship it with the binaries.
- Add `cargo-deny` with an allow-list of licenses so a copyleft dependency (GPL and the like) cannot slip in unnoticed. Dual-licensed crates such as `r-efi` and `self_cell` are fine through their permissive option.
- Neither tool is installed yet; ask before installing.

### 18. UI polish from the design critique (ui)
Source: a design review of screenshots on 2026-10-20 (Operate mode; rough Nielsen score 25/40). Every item changes the UI, so each starts with an ASCII mockup and waits for confirmation. The suggested order is the first three, which pay off most; the rest follow as wanted.
- 18a. (done) Separate the toolbar from the list: the toolbar sits on the theme's `secondary` surface with a bottom border drawn from the text color at 22% (the theme's own hairline was too faint in the dark theme).
- 18b. (done: Apply and Clear all on the Level row, Level chips instead of checkboxes, an All | WARN+ segmented control, small aligned labels) Tighten the toolbar (about 230 px and four rows today): keep `Apply` / `Clear all` from wrapping onto their own line; give the row labels (`Level`, `Text`, `Highlight`) the same small size as the controls instead of larger; consider Level chips that toggle directly (no checkboxes) and `All | WARN+` as one segmented control.
- 18c. Row color in the Entry view: replace the heavy full-row WARN tint (olive in the dark theme) with a 3 px severity stripe plus a faint tint, and keep the stripe when a row is selected (selection currently hides the Level color).
- 18d. One on/off style for every toggle (`Aa`, `.*`, `¬`, `All`, `WARN+`); show an "unapplied" hint when the Filter inputs changed but `Apply` was not pressed.
- 18e. Detail panel: move `Copy entry` into the header next to `×`; show time and size in the header; avoid breaking lines in the middle of a token such as `path=/api/orders`; make the resize edge discoverable.
- 18f. Small fixes: the Settings `Context` hint is clipped; thousands separators in the Search results header (`61,108`, as in the Search bar); the footer `plain text ▾` looks like a menu but toggles the Format; tooltips overlapping the Level row; make the Follow button more visible.
- Still open after this ticket, in order: 14b, 15 (keyboard shortcuts), 16 (Bookmarks), 17 (license notices).
