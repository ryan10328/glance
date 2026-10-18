//! One open Log File: its Filter, selection, scrolling and Entry view.

use std::cell::Cell;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glance_core::entry;
use glance_core::filter::{CompiledFilter, FilterContext, FilteredView, ViewRow, };
use glance_core::format::{parse_entry, resolve_time_zone};
use glance_core::highlight::{Highlighter, Rgb, palette_rgb};
use glance_core::search::{AllEntries, Direction, Rows, Search, SearchQuery};
use glance_core::selection::Selection;
use glance_core::session::{LogFile, Polled};
use glance_core::settings::Config;
use glance_core::watch::Rotation;
use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::tz::TimeZone;

use crate::filter_bar::{FilterBar, FilterBarEvent};
use crate::highlight_bar::HighlightBar;
use crate::level_style::{LevelPalette, LevelStyle};
use crate::search_bar::{SearchBar, SearchBarEvent};

actions!(glance, [CopySelection, OpenSearch, CloseSearch]);

const DEFAULT_FONT_SIZE: f32 = 13.0;
const GUTTER_WIDTH: f32 = 72.0;
/// Largest selection that can be copied at once.
const COPY_LIMIT: usize = 64 * 1024 * 1024;
/// Entries examined per step while a Filter catches up with the index.
const FILTER_STEP: usize = 20_000;
/// Longest time one `work` call keeps the UI thread busy.
const WORK_BUDGET: Duration = Duration::from_millis(8);

#[cfg(target_os = "macos")]
const DEFAULT_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const DEFAULT_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const DEFAULT_FONT: &str = "monospace";

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
    search_open: bool,
    /// The query as typed; kept so Search can restart when the rows change.
    search_query: Option<SearchQuery>,
    search: Option<Search>,
    search_invalid: bool,
    /// The row of the match the user is looking at.
    search_current: Option<usize>,
    /// Jump to the first match once one is found (after the query changed).
    search_autojump: bool,
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
            let zone = resolve_time_zone(None, config.default_time_zone.as_deref()).unwrap_or(TimeZone::UTC);
            let rule = file.index().read().unwrap().rule();
            let ctx = FilterContext { format: file.format(), rule, zone };
            let filter_bar = cx.new(|cx| FilterBar::new(config.filter_context, window, cx));
            cx.subscribe(&filter_bar, |this: &mut TabView, _, event: &FilterBarEvent, cx| {
                let FilterBarEvent::Apply(filter) = event;
                this.apply_filter(filter, cx);
            })
            .detach();
            let search_bar = cx.new(|cx| SearchBar::new(window, cx));
            cx.subscribe_in(&search_bar, window, |this: &mut TabView, _, event: &SearchBarEvent, window, cx| match event {
                SearchBarEvent::Changed(query) => this.set_search_query(query.clone(), cx),
                SearchBarEvent::Next => this.step_search(Direction::Forward, cx),
                SearchBarEvent::Previous => this.step_search(Direction::Backward, cx),
                SearchBarEvent::Close => this.close_search(window, cx),
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
                search_open: false,
                search_query: None,
                search: None,
                search_invalid: false,
                search_current: None,
                search_autojump: false,
            }
        })
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

    // ---- Search -------------------------------------------------------------

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search_bar.update(cx, |bar, cx| bar.focus(window, cx));
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
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

    fn publish_search_status(&mut self, cx: &mut Context<Self>) {
        let empty = self.search_query.as_ref().is_none_or(|q| q.pattern.is_empty());
        let (status, invalid) = match &self.search {
            _ if self.search_invalid => ("invalid regex".to_owned(), true),
            Some(search) if !empty => {
                let ordinal = self.search_current.and_then(|r| search.ordinal(r)).map_or("–".to_owned(), |o| o.to_string());
                let more = if search.scanned_rows() < self.row_count() { "…" } else { "" };
                (format!("{ordinal} / {}{more}", search.count()), false)
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
            Ok(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Err(e) => self.message = Some(e.to_string()),
        }
        drop(index);
        cx.notify();
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
        let list_scroll = self.list_scroll.clone();
        let family: SharedString = self.config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let font_size = self.font_size();
        let row_height = (font_size * 1.5).ceil();
        let palette = LevelPalette::new(cx);
        let dark = cx.theme().is_dark();
        let highlighter = self.highlighter.clone();
        let (muted, selected_bg, hover_bg, current_bg, found_bg) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.list_active, theme.list_hover, theme.accent, theme.warning)
        };
        let list = uniform_list(
            "entries",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let index = index.read().unwrap();
                range
                    .filter_map(|row| {
                        let view_row = match &this.filter {
                            Some((_, view)) => view.row(row)?,
                            None => ViewRow { entry: row, is_match: true },
                        };
                        let entry_no = view_row.entry;
                        let summary = entry::read_summary(&index, &reader, entry_no).ok().flatten();
                        let (text, extra, more) =
                            summary.map_or((String::new(), 0, false), |s| (s.first_line, s.extra_lines, s.more));
                        max_chars.set(max_chars.get().max(text.chars().count() + 12));
                        let level = parse_entry(this.ctx.format, this.ctx.rule, text.as_bytes(), &this.ctx.zone).level;
                        let LevelStyle { tint, faded } = palette.style(level);
                        // Search matches get a background; the user's Highlight Rules color the text.
                        let search_ranges = this.search.as_ref().map(|s| s.occurrences(text.as_bytes())).unwrap_or_default();
                        let found = merge_highlights(&text, search_ranges, highlighter.text_spans(text.as_bytes()), dark, found_bg.opacity(0.55));
                        let badge = (extra > 0).then(|| format!("  [+{extra}{} lines]", if more { "+" } else { "" }));
                        Some(
                            h_flex()
                                .id(("row", row))
                                .h(px(row_height))
                                .w_full()
                                .flex_none()
                                .when(!view_row.is_match, |s| s.opacity(0.55))
                                .when_some(tint, |s, tint| s.bg(tint))
                                .when(selection.contains(entry_no), |s| s.bg(selected_bg))
                                .when(this.search_current == Some(row), |s| s.bg(current_bg))
                                .hover(|s| s.bg(hover_bg))
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
                                        div()
                                            .relative()
                                            .left(px(-scroll_x))
                                            .whitespace_nowrap()
                                            .when(faded, |s| s.text_color(muted))
                                            .child(StyledText::new(text).with_highlights(found))
                                            .children(badge.map(|b| div().text_color(muted).child(b))),
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
            .child(
                div().absolute().top_0().right_0().bottom_0().w(Scrollbar::width()).child(
                    Scrollbar::vertical(&list_scroll).mode(ScrollbarMode::Always).viewport_from_layout(),
                ),
            )
    }
}

impl Render for TabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .on_action(cx.listener(|this, _: &OpenSearch, window, cx| this.open_search(window, cx)))
            .on_action(cx.listener(|this, _: &CloseSearch, window, cx| this.close_search(window, cx)))
            .children(self.render_rotation_bar(cx))
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(self.filter_bar.clone())
                    .children(self.search_open.then(|| self.search_bar.clone()))
                    .child(self.highlight_bar.clone()),
            )
            .child(self.render_entry_view(cx))
    }
}
