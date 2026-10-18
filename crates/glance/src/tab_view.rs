//! One open Log File: its Filter, selection, scrolling and Entry view.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glance_core::entry;
use glance_core::filter::{CompiledFilter, FilterContext, FilteredView, ViewRow, time_filter_available};
use glance_core::format::{Format, resolve_time_zone};
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

actions!(glance, [CopySelection]);

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
}

impl TabView {
    pub fn new(file: LogFile, config: Rc<Config>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let zone = resolve_time_zone(None, config.default_time_zone.as_deref()).unwrap_or(TimeZone::UTC);
            let rule = file.index().read().unwrap().rule();
            let ctx = FilterContext { format: file.format(), rule, zone: zone.clone() };
            let json = file.format() == Format::JsonLines;
            let time_available = time_filter_available(ctx.format, ctx.rule);
            let filter_bar = cx.new(|cx| FilterBar::new(json, time_available, zone, window, cx));
            cx.subscribe(&filter_bar, |this: &mut TabView, _, event: &FilterBarEvent, cx| {
                let FilterBarEvent::Apply(filter) = event;
                this.apply_filter(filter, cx);
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
                message: None,
            }
        })
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
        let Some((compiled, view)) = &mut self.filter else { return false };
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
        let summary = format!(
            "{} of {} entries{}",
            view.len(),
            total,
            if caught_up { String::new() } else { format!(" · filtering {}%", examined * 100 / total.max(1)) }
        );
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
        self.filter_bar.update(cx, |bar, cx| bar.set_summary(summary, cx));
        if changed {
            cx.notify();
        }
        more
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
        let (muted, selected_bg, hover_bg) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.list_active, theme.list_hover)
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
                        let badge = (extra > 0).then(|| format!("  [+{extra}{} lines]", if more { "+" } else { "" }));
                        Some(
                            h_flex()
                                .id(("row", row))
                                .h(px(row_height))
                                .w_full()
                                .flex_none()
                                .when(!view_row.is_match, |s| s.opacity(0.55))
                                .when(selection.contains(entry_no), |s| s.bg(selected_bg))
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
                                            .child(text)
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
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .children(self.render_rotation_bar(cx))
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(self.filter_bar.clone())
                    .child(div().text_color(muted).child("Search: (coming in the next step)")),
            )
            .child(self.render_entry_view(cx))
    }
}
