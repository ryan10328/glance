//! One open Log File: its Filter, selection, scrolling and Entry view.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glance_core::entry::{self, EntryDetail};
use glance_core::filter::{CompiledFilter, FilterContext, FilteredView, TextPattern, ViewRow};
use glance_core::format::{parse_entry, resolve_time_zone};
use glance_core::highlight::{Highlighter, Rgb, palette_rgb};
use glance_core::search::{AllEntries, Direction, Rows, Search, SearchQuery};
use glance_core::selection::Selection;
use glance_core::session::{LogFile, Polled};
use glance_core::settings::Config;
use glance_core::watch::Rotation;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::tz::TimeZone;

use crate::filter_bar::{FilterBar, FilterBarEvent};
use crate::highlight_bar::HighlightBar;
use crate::level_style::{LevelPalette, LevelStyle, RowStyle};
use crate::search_bar::{SearchBar, SearchBarEvent};

actions!(glance, [CopySelection, OpenSearch, CloseSearch]);

pub const DEFAULT_FONT_SIZE: f32 = 13.0;
const GUTTER_WIDTH: f32 = 72.0;
/// Lines of one Entry the detail panel renders.
const DETAIL_MAX_LINES: usize = 5000;
/// Largest selection that can be copied at once.
const COPY_LIMIT: usize = 64 * 1024 * 1024;
/// Entries examined per step while a Filter catches up with the index.
const FILTER_STEP: usize = 20_000;
/// Longest time one `work` call keeps the UI thread busy.
const WORK_BUDGET: Duration = Duration::from_millis(8);

#[cfg(target_os = "macos")]
pub const DEFAULT_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const DEFAULT_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const DEFAULT_FONT: &str = "monospace";

/// A frozen list of Search matches shown in the panel below the Entry view.
struct Results {
    /// The query as typed, for the header.
    label: String,
    /// Matching Entry numbers, ascending. Entries (not rows) so a later Filter change cannot shift them.
    entries: Vec<usize>,
    /// Finds the occurrences to color in each result.
    search: Search,
}

/// Follow Tail as the user sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowState {
    Off,
    Following,
    /// Following is on but the user scrolled away from the bottom.
    Paused,
}

pub struct TabView {
    file: LogFile,
    config: Rc<Config>,
    highlighter: Rc<Highlighter>,
    /// Shared by every Tab; shown here so it sits with the Filter and Search bars.
    highlight_bar: Entity<HighlightBar>,
    ctx: FilterContext,
    selection: Selection,
    /// Horizontal scroll offset of the Entry text, in pixels.
    scroll_x: f32,
    /// Longest first line seen so far, in characters; bounds `scroll_x`.
    max_chars: Rc<Cell<usize>>,
    /// Vertical position of the Entry view, shared with its scrollbar.
    list_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    filter_bar: Entity<FilterBar>,
    /// `None` while no Filter is applied: every Entry is a row.
    filter: Option<(CompiledFilter, FilteredView)>,
    /// After a Filter change, scroll to this Entry once its row is known.
    pending_scroll: Option<usize>,
    /// The Filter finished and the selection has been reconciled with it.
    selection_settled: bool,
    generation: u64,
    message: Option<String>,
    search_bar: Entity<SearchBar>,
    /// The query as typed; kept so Search can restart when the rows change.
    search_query: Option<SearchQuery>,
    search: Option<Search>,
    search_invalid: bool,
    /// The row of the match the user is looking at.
    search_current: Option<usize>,
    /// Jump to the first match once one is found (after the query changed).
    search_autojump: bool,
    /// "Find all" was requested; the list is taken once Search has covered every row.
    find_pending: bool,
    results: Option<Results>,
    results_scroll: UniformListScrollHandle,
    /// The result the user last clicked.
    results_current: Option<usize>,
    split: Entity<ResizableState>,
    follow: bool,
    follow_paused: bool,
    /// Rows at the last scroll to the bottom; `usize::MAX` forces a scroll.
    follow_rows: usize,
    /// Render count at that scroll, so a stale view is not mistaken for the user scrolling.
    follow_render: usize,
    /// (renders so far, end of the visible row range) as of the last Entry view render.
    view_state: Rc<Cell<(usize, usize)>>,
    detail_split: Entity<ResizableState>,
    /// The user closed the detail panel for the current selection.
    detail_hidden: bool,
    /// The Entry the panel last showed, to reopen it when the selection moves on.
    detail_for: Option<usize>,
    /// (Entry, file generation, content) of the last Entry read for the panel.
    detail_cache: RefCell<Option<(usize, u64, Rc<EntryDetail>)>>,
    detail_scroll: ScrollHandle,
    /// Entries just copied (for the button's feedback), until `copy_token` moves on.
    copied: Option<usize>,
    copy_token: u64,
    copy_error: Option<String>,
    /// Field counts of long JSON Entries, for the row label; `None` when not countable.
    field_counts: RefCell<HashMap<usize, Option<usize>>>,
}

/// `81799` becomes `81,799`.
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn color_of([r, g, b]: Rgb) -> Hsla {
    rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)).into()
}

/// Combines Search matches (background) with Highlight Rule matches (text color)
/// into non-overlapping runs; where both cover the same text both apply.
fn merge_highlights(
    text: &str,
    search: Vec<Range<usize>>,
    rules: Vec<(Range<usize>, usize)>,
    dark: bool,
    search_bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let on_boundary = |r: &Range<usize>| text.is_char_boundary(r.start) && text.is_char_boundary(r.end);
    let search: Vec<_> = search.into_iter().filter(on_boundary).collect();
    let rules: Vec<_> = rules.into_iter().filter(|(r, _)| on_boundary(r)).collect();
    let mut cuts: Vec<usize> =
        search.iter().map(|r| [r.start, r.end]).chain(rules.iter().map(|(r, _)| [r.start, r.end])).flatten().collect();
    cuts.sort_unstable();
    cuts.dedup();
    let covers = |r: &Range<usize>, a: usize, b: usize| r.start <= a && b <= r.end;
    cuts.windows(2)
        .filter_map(|w| {
            let (a, b) = (w[0], w[1]);
            let background = search.iter().any(|r| covers(r, a, b));
            let color = rules.iter().find(|(r, _)| covers(r, a, b)).map(|(_, c)| color_of(palette_rgb(*c, dark)));
            (background || color.is_some()).then(|| {
                (a..b, HighlightStyle { background_color: background.then_some(search_bg), color, ..Default::default() })
            })
        })
        .collect()
}

impl TabView {
    pub fn new(file: LogFile, config: Rc<Config>, highlighter: Rc<Highlighter>, highlight_bar: Entity<HighlightBar>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let zone = resolve_time_zone(None, None).unwrap_or(TimeZone::UTC);
            let rule = file.index().read().unwrap().rule();
            let ctx = FilterContext { format: file.format(), rule, zone };
            let filter_bar = cx.new(|cx| FilterBar::new(config.filter_context, window, cx));
            cx.subscribe(&filter_bar, |this: &mut TabView, _, event: &FilterBarEvent, cx| {
                let FilterBarEvent::Apply(filter) = event;
                this.apply_filter(filter, cx);
            })
            .detach();
            let search_bar = cx.new(|cx| SearchBar::new(window, cx));
            cx.subscribe(&search_bar, |this: &mut TabView, _, event: &SearchBarEvent, cx| match event {
                SearchBarEvent::Changed(query) => this.set_search_query(query.clone(), cx),
                SearchBarEvent::Next => this.step_search(Direction::Forward, cx),
                SearchBarEvent::Previous => this.step_search(Direction::Backward, cx),
                SearchBarEvent::FindAll => this.find_all(cx),
            })
            .detach();
            cx.spawn(async move |this, cx| {
                loop {
                    let busy = match this.update(cx, |this, cx| this.work(cx)) {
                        Ok(busy) => busy,
                        Err(_) => break,
                    };
                    cx.background_executor().timer(Duration::from_millis(if busy { 1 } else { 100 })).await;
                }
            })
            .detach();
            Self {
                message: None,
                highlighter,
                highlight_bar,
                generation: file.generation(),
                file,
                config,
                ctx,
                selection: Selection::default(),
                scroll_x: 0.0,
                max_chars: Rc::new(Cell::new(0)),
                list_scroll: UniformListScrollHandle::new(),
                focus: cx.focus_handle(),
                filter_bar,
                filter: None,
                pending_scroll: None,
                selection_settled: true,
                search_bar,
                search_query: None,
                search: None,
                search_invalid: false,
                search_current: None,
                search_autojump: false,
                find_pending: false,
                results: None,
                results_scroll: UniformListScrollHandle::new(),
                results_current: None,
                split: cx.new(|_| ResizableState::default()),
                follow: false,
                follow_paused: false,
                follow_rows: usize::MAX,
                follow_render: 0,
                view_state: Rc::new(Cell::new((0, 0))),
                detail_split: cx.new(|_| ResizableState::default()),
                detail_hidden: false,
                detail_for: None,
                detail_cache: RefCell::new(None),
                detail_scroll: ScrollHandle::new(),
                copied: None,
                copy_token: 0,
                copy_error: None,
                field_counts: RefCell::new(HashMap::new()),
            }
        })
    }

    /// The settings changed: font, size and Filter context apply at once.
    pub fn set_config(&mut self, config: Rc<Config>, cx: &mut Context<Self>) {
        let context = config.filter_context;
        self.config = config;
        self.filter_bar.update(cx, |bar, cx| bar.set_context(context, cx));
        cx.notify();
    }

    /// The Highlight Rules changed in the window.
    pub fn set_highlighter(&mut self, highlighter: Rc<Highlighter>, cx: &mut Context<Self>) {
        self.highlighter = highlighter;
        cx.notify();
    }

    pub fn title(&self) -> String {
        let path = self.file.path();
        path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn file(&self) -> &LogFile {
        &self.file
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Entries shown (after the Filter) and Entries in the file so far.
    pub fn counts(&self) -> (usize, usize) {
        let total = self.file.index().read().unwrap().len();
        (self.filter.as_ref().map_or(total, |(_, view)| view.len()), total)
    }

    /// Percent of the file the Filter has examined, or `None` once it has caught up (or without a Filter).
    pub fn filter_progress(&self) -> Option<usize> {
        let (_, view) = self.filter.as_ref()?;
        let total = self.file.index().read().unwrap().len();
        let examined = view.examined();
        (examined < total.saturating_sub(1)).then(|| examined * 100 / total.max(1))
    }

    // ---- change detection --------------------------------------------------

    pub fn poll(&mut self, cx: &mut Context<Self>) -> std::io::Result<Polled> {
        let polled = self.file.poll()?;
        if self.file.generation() != self.generation {
            self.generation = self.file.generation();
            self.reset_after_reload(cx);
        }
        if polled != Polled::Unchanged {
            cx.notify();
        }
        Ok(polled)
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match self.file.reload() {
            Ok(()) => {
                self.generation = self.file.generation();
                self.reset_after_reload(cx);
            }
            Err(e) => self.message = Some(format!("Cannot reload: {e}")),
        }
        cx.notify();
    }

    fn reset_after_reload(&mut self, _cx: &mut Context<Self>) {
        self.selection.clear();
        self.results = None;
        self.find_pending = false;
        self.field_counts.borrow_mut().clear();
        self.scroll_x = 0.0;
        self.max_chars.set(0);
        if let Some((compiled, _)) = self.filter.take() {
            self.filter = Some((compiled, FilteredView::new()));
            self.selection_settled = false;
        }
        self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.start_search(false);
    }

    // ---- Filter -------------------------------------------------------------

    fn apply_filter(&mut self, filter: &glance_core::filter::Filter, cx: &mut Context<Self>) {
        // Remember the Entry at the top so the view can stay near it.
        let top_row = self.list_scroll.0.borrow().base_handle.logical_scroll_top().0;
        let top_entry = self.row_at(top_row).map(|r| r.entry);
        if *filter == glance_core::filter::Filter::default() {
            self.filter = None;
            self.filter_bar.update(cx, |bar, cx| bar.set_error(None, cx));
            self.pending_scroll = None;
            if let Some(entry) = top_entry {
                self.list_scroll.scroll_to_item_strict(entry, ScrollStrategy::Top);
            }
        } else {
            match CompiledFilter::new(filter) {
                Ok(compiled) => {
                    self.filter = Some((compiled, FilteredView::new()));
                    self.selection_settled = false;
                    self.pending_scroll = top_entry;
                    self.filter_bar.update(cx, |bar, cx| bar.set_error(None, cx));
                }
                Err(e) => self.filter_bar.update(cx, |bar, cx| bar.set_error(Some(e.to_string()), cx)),
            }
        }
        self.start_search(false);
        cx.notify();
    }

    fn row_count(&self) -> usize {
        match &self.filter {
            Some((_, view)) => view.len(),
            None => self.file.index().read().unwrap().len(),
        }
    }

    fn row_at(&self, row: usize) -> Option<ViewRow> {
        match &self.filter {
            Some((_, view)) => view.row(row),
            None => (row < self.file.index().read().unwrap().len()).then_some(ViewRow { entry: row, is_match: true }),
        }
    }

    /// Advances background work a little. Returns whether there is more to do.
    fn work(&mut self, cx: &mut Context<Self>) -> bool {
        self.work_follow(cx);
        let filtering = self.work_filter(cx);
        let searching = self.work_search(cx);
        filtering || searching
    }

    fn work_filter(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((compiled, view)) = &mut self.filter else {
            return false;
        };
        let started = Instant::now();
        let before = view.len();
        let mut more = false;
        {
            let index = self.file.index().read().unwrap();
            loop {
                match view.update(compiled, &index, self.file.reader(), &self.ctx, FILTER_STEP) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        self.message = Some(e.to_string());
                        break;
                    }
                }
                if started.elapsed() > WORK_BUDGET {
                    more = true;
                    break;
                }
            }
        }
        let (examined, total) = (view.examined(), self.file.index().read().unwrap().len());
        let caught_up = !more && examined >= total.saturating_sub(1);
        let changed = view.len() != before || more;
        if let Some(entry) = self.pending_scroll.filter(|&entry| examined > entry || caught_up) {
            let row = self.filter.as_ref().map_or(0, |(_, v)| v.first_row_at_or_after(entry));
            self.list_scroll.scroll_to_item_strict(row, ScrollStrategy::Top);
            self.pending_scroll = None;
        }
        if caught_up && !self.selection_settled {
            self.selection_settled = true;
            if let Some((_, view)) = &self.filter {
                self.selection.retain(|e| view.row_of(e).is_some());
            }
            cx.notify();
        }
        if changed {
            cx.notify();
        }
        more
    }

    // ---- Follow Tail ---------------------------------------------------------

    pub fn follow_state(&self) -> FollowState {
        match (self.follow, self.follow_paused) {
            (false, _) => FollowState::Off,
            (true, false) => FollowState::Following,
            (true, true) => FollowState::Paused,
        }
    }

    /// Off turns Following on, Following turns it off, Paused resumes it.
    pub fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        match self.follow_state() {
            FollowState::Off => {
                self.follow = true;
                self.resume_follow(cx);
            }
            FollowState::Following => {
                self.follow = false;
                self.follow_paused = false;
            }
            FollowState::Paused => self.resume_follow(cx),
        }
        cx.notify();
    }

    fn resume_follow(&mut self, cx: &mut Context<Self>) {
        self.follow_paused = false;
        self.follow_rows = usize::MAX;
        cx.notify();
    }

    /// Keeps the view at the bottom while new Entries arrive, and pauses when the user scrolls up.
    fn work_follow(&mut self, cx: &mut Context<Self>) {
        if !self.follow {
            return;
        }
        if self.file.rotated().is_some() {
            self.follow = false;
            self.follow_paused = false;
            cx.notify();
            return;
        }
        let rows = self.row_count();
        let (renders, visible_end) = self.view_state.get();
        let fresh = renders > self.follow_render;
        if self.follow_paused {
            if fresh && rows > 0 && visible_end >= rows {
                self.follow_paused = false;
                self.follow_rows = rows;
                cx.notify();
            }
            return;
        }
        if fresh && self.follow_rows != usize::MAX && visible_end < self.follow_rows {
            self.follow_paused = true;
            cx.notify();
        } else if rows != self.follow_rows {
            self.follow_rows = rows;
            self.follow_render = renders;
            self.list_scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    // ---- Search -------------------------------------------------------------

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |bar, cx| bar.focus(window, cx));
        cx.notify();
    }

    /// Clears the query and returns to the Entry view; the Search bar itself stays.
    fn cancel_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_bar.update(cx, |bar, cx| bar.clear(window, cx));
        self.search_query = None;
        self.search = None;
        self.search_invalid = false;
        self.search_current = None;
        self.search_autojump = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn set_search_query(&mut self, query: SearchQuery, cx: &mut Context<Self>) {
        self.search_query = Some(query);
        self.find_pending = false;
        self.start_search(true);
        self.work_search(cx);
        cx.notify();
    }

    /// (Re)starts Search over the current rows. `jump` moves to the first match.
    fn start_search(&mut self, jump: bool) {
        self.search_current = None;
        self.search_autojump = jump;
        let Some(query) = self.search_query.clone() else {
            self.search = None;
            return;
        };
        match Search::new(&query) {
            Ok(search) => {
                self.search = Some(search);
                self.search_invalid = false;
            }
            Err(_) => {
                self.search = None;
                self.search_invalid = true;
            }
        }
    }

    fn top_row(&self) -> usize {
        self.list_scroll.0.borrow().base_handle.logical_scroll_top().0
    }

    fn step_search(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let Some(search) = &self.search else { return };
        let from = self.search_current.or_else(|| match direction {
            Direction::Forward => self.top_row().checked_sub(1),
            Direction::Backward => Some(self.top_row()),
        });
        if let Some(hit) = search.step(from, direction, true) {
            self.search_current = Some(hit.row);
            self.list_scroll.scroll_to_item_strict(hit.row, ScrollStrategy::Center);
        }
        self.search_autojump = false;
        self.publish_search_status(cx);
        cx.notify();
    }

    /// Scans a little further. Returns whether there is more to do.
    fn work_search(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(search) = &mut self.search else {
            self.find_pending = false;
            self.publish_search_status(cx);
            return false;
        };
        let started = Instant::now();
        let before = search.count();
        let mut more = false;
        {
            let index = self.file.index().read().unwrap();
            let all = AllEntries(&index);
            let rows: &dyn Rows = match &self.filter {
                Some((_, view)) => view,
                None => &all,
            };
            loop {
                match search.update(rows, &index, self.file.reader(), FILTER_STEP) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        self.message = Some(e.to_string());
                        break;
                    }
                }
                if started.elapsed() > WORK_BUDGET {
                    more = true;
                    break;
                }
            }
        }
        let grew = search.count() != before;
        let caught_up = !more && search.scanned_rows() >= self.row_count();
        if self.find_pending && caught_up {
            self.finish_find_all();
        }
        if self.search_autojump && (grew || !more) {
            self.step_search(Direction::Forward, cx);
        } else {
            self.publish_search_status(cx);
            if grew {
                cx.notify();
            }
        }
        more
    }

    /// Lists every match in the panel; waits for Search to cover all rows first.
    fn find_all(&mut self, cx: &mut Context<Self>) {
        if self.search.is_none() || self.search_query.as_ref().is_none_or(|q| q.pattern.is_empty()) {
            return;
        }
        self.find_pending = true;
        self.work_search(cx);
        cx.notify();
    }

    fn finish_find_all(&mut self) {
        let (Some(search), Some(query)) = (&self.search, &self.search_query) else { return };
        let Ok(occurrences) = Search::new(query) else { return };
        let entries = search.matches().filter_map(|row| self.row_at(row).map(|r| r.entry)).collect();
        let label = match &query.pattern {
            TextPattern::Wildcard(text) | TextPattern::Regex(text) => text.clone(),
        };
        self.results = Some(Results { label, entries, search: occurrences });
        self.results_current = None;
        self.find_pending = false;
    }

    /// Selects an Entry and scrolls the Entry view to it, e.g. after a click in the results panel.
    fn jump_to_entry(&mut self, entry: usize, cx: &mut Context<Self>) {
        let row = match &self.filter {
            Some((_, view)) => view.row_of(entry),
            None => Some(entry),
        };
        let Some(row) = row else {
            self.message = Some("That Entry is hidden by the current Filter".to_owned());
            cx.notify();
            return;
        };
        self.message = None;
        self.selection.click(entry);
        if self.search.as_ref().is_some_and(|s| s.ordinal(row).is_some()) {
            self.search_current = Some(row);
            self.publish_search_status(cx);
        }
        self.list_scroll.scroll_to_item_strict(row, ScrollStrategy::Center);
        cx.notify();
    }

    fn publish_search_status(&mut self, cx: &mut Context<Self>) {
        let empty = self.search_query.as_ref().is_none_or(|q| q.pattern.is_empty());
        let (status, invalid) = match &self.search {
            _ if self.search_invalid => ("invalid regex".to_owned(), true),
            Some(search) if !empty => {
                let ordinal = self.search_current.and_then(|r| search.ordinal(r)).map_or("–".to_owned(), group_digits);
                let more = if search.scanned_rows() < self.row_count() { "…" } else { "" };
                (format!("{ordinal} / {}{more}", group_digits(search.count())), false)
            }
            _ => (String::new(), false),
        };
        self.search_bar.update(cx, |bar, cx| bar.set_status(status, invalid, cx));
    }

    // ---- selection ----------------------------------------------------------

    fn select_row(&mut self, row: usize, modifiers: Modifiers) {
        let Some(clicked) = self.row_at(row) else { return };
        if modifiers.shift {
            let anchor_row = self
                .selection
                .anchor()
                .and_then(|entry| match &self.filter {
                    Some((_, view)) => view.row_of(entry),
                    None => Some(entry),
                })
                .unwrap_or(row);
            let (lo, hi) = (anchor_row.min(row), anchor_row.max(row));
            let entries: Vec<usize> = (lo..=hi).filter_map(|r| self.row_at(r).map(|v| v.entry)).collect();
            self.selection.replace_with(entries);
        } else if modifiers.secondary() {
            self.selection.toggle(clicked.entry);
        } else {
            self.selection.click(clicked.entry);
        }
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }
        let index = self.file.index().read().unwrap();
        match entry::copy_entries(&index, self.file.reader(), self.selection.rows(), COPY_LIMIT) {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.copy_error = None;
                self.copied = Some(self.selection.len());
                self.copy_token += 1;
                let token = self.copy_token;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.copy_token == token {
                            this.copied = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Err(e) => {
                self.copied = None;
                self.copy_error = Some(e.to_string());
                self.message = Some(e.to_string());
            }
        }
        drop(index);
        cx.notify();
    }

    /// Label for a long JSON Entry that the row cannot show whole (a cut first line).
    fn json_label(&self, index: &glance_core::index::EntryIndex, entry_no: usize) -> String {
        let cached = self.field_counts.borrow().get(&entry_no).copied();
        let count = cached.unwrap_or_else(|| {
            let count = entry::json_field_count(index, self.file.reader(), entry_no).ok().flatten();
            self.field_counts.borrow_mut().insert(entry_no, count);
            count
        });
        match count {
            Some(n) => format!("{{ }} {n} fields"),
            None => "{ } …".to_owned(),
        }
    }

    fn scroll_horizontally(&mut self, dx: f32, font_size: f32) {
        // Monospace glyphs are about 0.6 em wide.
        let content = self.max_chars.get() as f32 * font_size * 0.6;
        self.scroll_x = (self.scroll_x - dx).clamp(0.0, (content - 200.0).max(0.0));
    }

    // ---- rendering ----------------------------------------------------------

    fn font_size(&self) -> f32 {
        self.config.font_size.filter(|s| *s >= 6.0).unwrap_or(DEFAULT_FONT_SIZE)
    }

    fn render_rotation_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let (bg, fg) = (theme.warning, theme.warning_foreground);
        self.file.rotated().map(|rotation| {
            let what = match rotation {
                Rotation::Replaced => "was replaced",
                Rotation::Truncated => "was truncated",
                Rotation::Removed => "was removed",
            };
            h_flex()
                .px_3()
                .py_1()
                .gap_3()
                .items_center()
                .bg(bg)
                .text_color(fg)
                .child(format!("⚠ This file {what}. Following has stopped."))
                .child(Button::new("reload").small().label("Reload").on_click(cx.listener(|this, _, _, cx| this.reload(cx))))
        })
    }

    fn render_entry_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.row_count();
        let index = self.file.index().clone();
        let reader = self.file.reader().clone();
        let selection = self.selection.clone();
        let scroll_x = self.scroll_x;
        let max_chars = self.max_chars.clone();
        let view_state = self.view_state.clone();
        let list_scroll = self.list_scroll.clone();
        let family: SharedString = self.config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let font_size = self.font_size();
        let row_height = (font_size * 1.5).ceil();
        let palette = LevelPalette::new(cx);
        let dark = cx.theme().is_dark();
        let highlighter = self.highlighter.clone();
        // Selection is blue, which no Level uses (WARN is yellow, ERROR and FATAL red).
        let (muted, selected_bg, selected_edge, hover_bg, found_bg, badge_bg, foreground) = {
            let theme = cx.theme();
            let (fill, edge) = if dark { (0.32, 0.8) } else { (0.22, 0.7) };
            (theme.muted_foreground, theme.info.opacity(fill), theme.info.opacity(edge), theme.list_hover, theme.warning, theme.muted, theme.foreground)
        };
        let list = uniform_list(
            "entries",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let index = index.read().unwrap();
                view_state.set((view_state.get().0 + 1, range.end));
                range
                    .filter_map(|row| {
                        let view_row = match &this.filter {
                            Some((_, view)) => view.row(row)?,
                            None => ViewRow { entry: row, is_match: true },
                        };
                        let entry_no = view_row.entry;
                        let selected = selection.contains(entry_no);
                        // The Entry index is already read-locked here, so neighbors are found without `row_at`
                        // (taking a second read lock can deadlock against the indexing thread).
                        let neighbor_selected = |r: usize| {
                            let entry = match &this.filter {
                                Some((_, view)) => view.row(r).map(|v| v.entry),
                                None => (r < count).then_some(r),
                            };
                            entry.is_some_and(|e| selection.contains(e))
                        };
                        let prev_selected = row > 0 && neighbor_selected(row - 1);
                        let next_selected = neighbor_selected(row + 1);
                        let summary = entry::read_summary(&index, &reader, entry_no).ok().flatten();
                        let (text, extra, more, cut) =
                            summary.map_or((String::new(), 0, false, false), |s| (s.first_line, s.extra_lines, s.more, s.truncated));
                        max_chars.set(max_chars.get().max(text.chars().count() + 12));
                        let level = parse_entry(this.ctx.format, this.ctx.rule, text.as_bytes(), &this.ctx.zone).level;
                        let RowStyle { tint, stripe, faded } = palette.row_style(level);
                        // Search matches get a background; the user's Highlight Rules color the text.
                        let search_ranges = this.search.as_ref().map(|s| s.occurrences(text.as_bytes())).unwrap_or_default();
                        let found = merge_highlights(&text, search_ranges, highlighter.text_spans(text.as_bytes()), dark, found_bg.opacity(0.55));
                        // A hint that the detail panel has more: continuation lines, or a JSON object too long for the row.
                        let badge = if extra > 0 {
                            Some(format!("⋯ +{extra}{} lines", if more { "+" } else { "" }))
                        } else if cut && this.ctx.format == glance_core::format::Format::JsonLines {
                            Some(this.json_label(&index, entry_no))
                        } else {
                            None
                        };
                        Some(
                            h_flex()
                                .id(("row", row))
                                .relative()
                                .h(px(row_height))
                                .w_full()
                                .flex_none()
                                .when(!view_row.is_match, |s| s.opacity(0.55))
                                .when_some(tint, |s, tint| s.bg(tint))
                                .when(selected, |s| s.bg(selected_bg))
                                // A selection gets an edge line where it starts and ends, so a block reads as one.
                                .when(selected && !prev_selected, |s| s.border_t_1().border_color(selected_edge))
                                .when(selected && !next_selected, |s| s.border_b_1().border_color(selected_edge))
                                .when(!selected && this.search_current == Some(row), |s| s.border_y_1().border_color(muted.opacity(0.6)))
                                .when(!selected, |s| s.hover(|s| s.bg(hover_bg)))
                                .children(stripe.map(|(color, width)| div().absolute().left_0().top_0().bottom_0().w(px(width)).bg(color)))
                                // The match Search is on: a marker next to the line number, no background.
                                .children((this.search_current == Some(row)).then(|| {
                                    div().absolute().left(px(5.)).top_0().bottom_0().flex().items_center().text_color(foreground).child("▸")
                                }))
                                .child(
                                    div()
                                        .w(px(GUTTER_WIDTH))
                                        .flex_none()
                                        .pr_3()
                                        .text_right()
                                        .text_color(muted)
                                        .child((entry_no + 1).to_string()),
                                )
                                .child(
                                    div().flex_1().h_full().overflow_hidden().child(
                                        h_flex()
                                            .relative()
                                            .left(px(-scroll_x))
                                            .items_center()
                                            .whitespace_nowrap()
                                            .when(faded, |s| s.text_color(muted))
                                            .child(StyledText::new(text).with_highlights(found))
                                            .children(badge.map(|b| div().ml_2().px_1().rounded(px(3.)).bg(badge_bg).text_color(muted).child(b))),
                                    ),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        window.focus(&this.focus, cx);
                                        this.select_row(row, event.modifiers);
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&list_scroll)
        .size_full();
        div()
            .id("entry-view")
            .relative()
            .flex_1()
            .min_h_0()
            .font_family(family)
            .text_size(px(font_size))
            .key_context("EntryView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let dx = f32::from(event.delta.pixel_delta(px(row_height)).x);
                if dx != 0.0 {
                    this.scroll_horizontally(dx, font_size);
                    cx.notify();
                }
            }))
            .child(list)
            .children((self.follow_state() == FollowState::Paused).then(|| {
                div().absolute().right(px(24.)).bottom(px(12.)).child(
                    Button::new("resume-follow")
                        .small()
                        .primary()
                        .label("↓ Resume Follow")
                        .tooltip("Scroll to the bottom and follow new Entries again")
                        .on_click(cx.listener(|this, _, _, cx| this.resume_follow(cx))),
                )
            }))
            .child(
                div().absolute().top_0().right_0().bottom_0().w(Scrollbar::width()).child(
                    Scrollbar::vertical(&list_scroll).mode(ScrollbarMode::Always).viewport_from_layout(),
                ),
            )
    }
}

impl TabView {
    /// The Entry the detail panel shows: the one last clicked, else any selected.
    fn detail_entry(&self) -> Option<usize> {
        self.selection.anchor().filter(|e| self.selection.contains(*e)).or_else(|| self.selection.rows().next())
    }

    fn detail_of(&self, entry: usize) -> Option<Rc<EntryDetail>> {
        if let Some((e, g, detail)) = &*self.detail_cache.borrow()
            && (*e, *g) == (entry, self.generation)
        {
            return Some(detail.clone());
        }
        let index = self.file.index().read().unwrap();
        let detail = Rc::new(entry::read_detail(&index, self.file.reader(), entry, self.ctx.format).ok().flatten()?);
        *self.detail_cache.borrow_mut() = Some((entry, self.generation, detail.clone()));
        Some(detail)
    }

    fn render_detail(&self, entry: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let detail = self.detail_of(entry)?;
        let first = detail.text.lines().next().unwrap_or("");
        let level = parse_entry(self.ctx.format, self.ctx.rule, first.as_bytes(), &self.ctx.zone).level;
        let level_label = level.map_or("No level", |l| l.as_str());
        let palette = LevelPalette::new(cx);
        let LevelStyle { tint, .. } = palette.style(level);
        let dark = cx.theme().is_dark();
        let (muted, border, header_bg, found_bg, foreground) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.secondary, theme.warning, theme.foreground)
        };
        let wrap = self.config.wrap_long_lines;
        let detail_scroll = self.detail_scroll.clone();
        let family: SharedString = self.config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let font_size = self.font_size();
        let line_height = (font_size * 1.5).ceil();
        let total_lines = detail.text.lines().count();
        let lines: Vec<AnyElement> = detail
            .text
            .lines()
            .take(DETAIL_MAX_LINES)
            .enumerate()
            .map(|(i, line)| {
                let search = self.search.as_ref().map(|s| s.occurrences(line.as_bytes())).unwrap_or_default();
                let found = merge_highlights(line, search, self.highlighter.text_spans(line.as_bytes()), dark, found_bg.opacity(0.55));
                div()
                    .id(("detail-line", i))
                    .min_h(px(line_height))
                    .when(wrap, |d| d.whitespace_normal())
                    .when(!wrap, |d| d.whitespace_nowrap())
                    .child(StyledText::new(line.to_owned()).with_highlights(found))
                    .into_any_element()
            })
            .collect();
        let selected = self.selection.len();
        let copy_label = match (self.copied, selected > 1) {
            (Some(n), true) => format!("✓ Copied {n} entries"),
            (Some(_), false) => "✓ Copied".to_owned(),
            (None, true) => format!("Copy {selected} entries"),
            (None, false) => "Copy entry".to_owned(),
        };
        let danger = cx.theme().danger;
        let notes = [
            (total_lines > DETAIL_MAX_LINES).then(|| format!("… {} more lines not shown", total_lines - DETAIL_MAX_LINES)),
            detail.truncated.then(|| "… the Entry is longer than what is shown".to_owned()),
        ];
        Some(
            v_flex()
                .size_full()
                .border_l_2()
                .border_color(border)
                .child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .justify_between()
                        .bg(header_bg)
                        .border_b_1()
                        .border_color(border)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Entry {}", entry + 1)))
                                .child(div().px_1().rounded(px(3.)).when_some(tint, |s, tint| s.bg(tint)).child(level_label))
                                .children((selected > 1).then(|| div().text_color(muted).child(format!("{selected} selected")))),
                        )
                        .child(Button::new("detail-close").xsmall().ghost().label("×").tooltip("Close the detail panel").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.detail_hidden = true;
                                cx.notify();
                            },
                        ))),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .id("detail-body")
                                .size_full()
                                // A row container lets the lines grow to their natural width when not wrapping,
                                // which is what makes the content wider than the panel and the bar appear.
                                .flex()
                                .items_start()
                                .overflow_scroll()
                                .track_scroll(&detail_scroll)
                                .p_2()
                                .font_family(family)
                                .text_size(px(font_size))
                                .text_color(foreground)
                                .child(
                                    v_flex()
                                        .when(wrap, |d| d.w_full())
                                        .when(!wrap, |d| d.min_w_full().flex_none())
                                        .children(lines)
                                        .children(notes.into_iter().flatten().map(|note| div().text_color(muted).child(note))),
                                ),
                        )
                        .child(
                            div().absolute().top_0().right_0().bottom_0().w(Scrollbar::width()).child(
                                Scrollbar::vertical(&detail_scroll).mode(ScrollbarMode::Always),
                            ),
                        )
                        .child(
                            div().absolute().left_0().right_0().bottom_0().h(Scrollbar::width()).child(
                                Scrollbar::horizontal(&detail_scroll).mode(ScrollbarMode::Always),
                            ),
                        ),
                )
                .child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(border)
                        .child({
                            let button = Button::new("detail-copy").xsmall().label(copy_label).tooltip("Copy the selected Entries (Cmd/Ctrl+C)");
                            let button = if self.copied.is_some() { button.primary() } else { button.outline() };
                            button.on_click(cx.listener(|this, _, _, cx| this.copy_selection(cx)))
                        })
                        .children(self.copy_error.clone().map(|e| div().text_color(danger).text_sm().child(e))),
                )
                .into_any_element(),
        )
    }

    fn render_results(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let results = self.results.as_ref()?;
        let count = results.entries.len();
        let header = format!("Search results · \"{}\" · {} match{}", results.label, count, if count == 1 { "" } else { "es" });
        let index = self.file.index().clone();
        let reader = self.file.reader().clone();
        let highlighter = self.highlighter.clone();
        let palette = LevelPalette::new(cx);
        let dark = cx.theme().is_dark();
        let family: SharedString = self.config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let font_size = self.font_size();
        let row_height = (font_size * 1.5).ceil();
        let (muted, border, hover_bg, current_bg, found_bg, header_bg, body_bg, foreground) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.border,
                theme.list_hover,
                theme.info.opacity(if theme.is_dark() { 0.32 } else { 0.22 }),
                theme.warning,
                theme.secondary,
                theme.muted.opacity(0.35),
                theme.foreground,
            )
        };
        let results_scroll = self.results_scroll.clone();
        let list = uniform_list(
            "results",
            count,
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                let Some(results) = &this.results else { return Vec::new() };
                let index = index.read().unwrap();
                range
                    .filter_map(|i| {
                        let entry_no = *results.entries.get(i)?;
                        let text = entry::read_summary(&index, &reader, entry_no).ok().flatten().map(|s| s.first_line).unwrap_or_default();
                        let level = parse_entry(this.ctx.format, this.ctx.rule, text.as_bytes(), &this.ctx.zone).level;
                        let RowStyle { tint, stripe, faded } = palette.row_style(level);
                        let found = merge_highlights(
                            &text,
                            results.search.occurrences(text.as_bytes()),
                            highlighter.text_spans(text.as_bytes()),
                            dark,
                            found_bg.opacity(0.55),
                        );
                        Some(
                            h_flex()
                                .id(("result", i))
                                .relative()
                                .h(px(row_height))
                                .w_full()
                                .flex_none()
                                .cursor_pointer()
                                .when_some(tint, |s, tint| s.bg(tint))
                                .when(this.results_current == Some(i), |s| s.bg(current_bg))
                                .hover(|s| s.bg(hover_bg))
                                .children(stripe.map(|(color, width)| div().absolute().left_0().top_0().bottom_0().w(px(width)).bg(color)))
                                .child(div().w(px(GUTTER_WIDTH)).flex_none().pr_3().text_right().text_color(muted).child((entry_no + 1).to_string()))
                                .child(
                                    div()
                                        .flex_1()
                                        .h_full()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .when(faded, |s| s.text_color(muted))
                                        .child(StyledText::new(text).with_highlights(found)),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        this.results_current = Some(i);
                                        this.jump_to_entry(entry_no, cx);
                                    }),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&results_scroll)
        .size_full();
        Some(
            v_flex()
                .size_full()
                .bg(body_bg)
                .border_t_2()
                .border_color(border)
                .child(
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_3()
                        .items_center()
                        .justify_between()
                        .bg(header_bg)
                        .border_b_1()
                        .border_color(border)
                        .text_color(foreground)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(header)
                        .child(Button::new("results-close").xsmall().ghost().label("×").tooltip("Close the results").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.results = None;
                                cx.notify();
                            },
                        ))),
                )
                .child(
                    div().relative().flex_1().min_h_0().font_family(family).text_size(px(font_size)).child(list).child(
                        div().absolute().top_0().right_0().bottom_0().w(Scrollbar::width()).child(
                            Scrollbar::vertical(&results_scroll).mode(ScrollbarMode::Always).viewport_from_layout(),
                        ),
                    ),
                )
                .into_any_element(),
        )
    }
}

impl Render for TabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The toolbar sits on its own surface; the theme's hairline is too faint to set it apart.
        let (toolbar_bg, toolbar_border) = {
            let theme = cx.theme();
            (theme.secondary, theme.foreground.opacity(0.22))
        };
        let shown = self.detail_entry();
        if shown != self.detail_for {
            self.detail_for = shown;
            self.detail_hidden = false;
        }
        let detail = shown.filter(|_| !self.detail_hidden).and_then(|entry| self.render_detail(entry, cx));
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &OpenSearch, window, cx| this.open_search(window, cx)))
            .on_action(cx.listener(|this, _: &CloseSearch, window, cx| this.cancel_search(window, cx)))
            .children(self.render_rotation_bar(cx))
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .bg(toolbar_bg)
                    .border_b_1()
                    .border_color(toolbar_border)
                    .child(self.filter_bar.clone())
                    .child(self.highlight_bar.clone())
                    .child(self.search_bar.clone()),
            )
            .child({
                let results = self.render_results(cx);
                let entry_view = self.render_entry_view(cx);
                let main_area = match results {
                    Some(results) => div()
                        .flex_1()
                        .min_h_0()
                        .child(
                            v_resizable("results-split")
                                .with_state(&self.split)
                                .child(resizable_panel().child(v_flex().size_full().child(entry_view)))
                                .child(resizable_panel().size(px(220.)).size_range(px(90.)..px(2000.)).child(results)),
                        )
                        .into_any_element(),
                    None => entry_view.into_any_element(),
                };
                match detail {
                    Some(detail) => div()
                        .flex_1()
                        .min_h_0()
                        .child(
                            h_resizable("detail-split")
                                .with_state(&self.detail_split)
                                .child(resizable_panel().child(v_flex().size_full().child(main_area)))
                                .child(resizable_panel().size(px(360.)).size_range(px(220.)..px(1400.)).child(detail)),
                        )
                        .into_any_element(),
                    None => main_area,
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::group_digits;

    #[test]
    fn digits_are_grouped_by_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(81799), "81,799");
        assert_eq!(group_digits(1234567), "1,234,567");
    }
}
