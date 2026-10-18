//! Window shell: Tab bar, status bar and the open-file flows.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use glance_core::entry;
use glance_core::format::Format;
use glance_core::selection::Selection;
use glance_core::session::{LogFile, Polled};
use glance_core::settings::{self, Config, State, WindowSize};
use glance_core::watch::Rotation;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

actions!(glance, [CopySelection]);

const POLL_TICK: Duration = Duration::from_millis(500);
/// Background Tabs are polled every this many ticks (about 2 s).
const BACKGROUND_EVERY: u64 = 4;
const MAX_RECENT: usize = 20;
const DEFAULT_FONT_SIZE: f32 = 13.0;
const GUTTER_WIDTH: f32 = 72.0;
/// Largest selection that can be copied at once.
const COPY_LIMIT: usize = 64 * 1024 * 1024;

#[cfg(target_os = "macos")]
const DEFAULT_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const DEFAULT_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const DEFAULT_FONT: &str = "monospace";

struct OpenTab {
    file: LogFile,
    selection: Selection,
    /// Horizontal scroll offset of the Entry text, in pixels.
    scroll_x: f32,
    /// Longest first line seen so far, in characters; bounds `scroll_x`.
    max_chars: Rc<Cell<usize>>,
}

impl OpenTab {
    fn new(file: LogFile) -> Self {
        Self { file, selection: Selection::default(), scroll_x: 0.0, max_chars: Rc::new(Cell::new(0)) }
    }

    fn title(&self) -> String {
        self.file
            .path()
            .file_name()
            .map_or_else(|| self.file.path().display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

pub struct GlanceApp {
    tabs: Vec<OpenTab>,
    active: usize,
    #[allow(dead_code)] // consumed by the Highlight and view tickets
    config: Config,
    state: State,
    state_dirty: bool,
    message: Option<String>,
    tick: u64,
    focus: FocusHandle,
}

fn settings_path(name: &str) -> Option<PathBuf> {
    settings::settings_dir().map(|dir| dir.join(name))
}

impl GlanceApp {
    pub fn load_state() -> State {
        settings_path(settings::STATE_FILE)
            .and_then(|p| settings::load::<State>(&p).ok())
            .unwrap_or_default()
    }

    pub fn new(args: Vec<PathBuf>, state: State, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (config, message) = match settings_path(settings::CONFIG_FILE).map(|p| settings::load::<Config>(&p)) {
            Some(Ok(config)) => (config, None),
            Some(Err(e)) => (Config::default(), Some(format!("config.toml ignored: {e}"))),
            None => (Config::default(), None),
        };
        let mut app = Self { tabs: Vec::new(), active: 0, config, state, state_dirty: false, message, tick: 0, focus: cx.focus_handle() };
        for path in args {
            app.open(&path, None);
        }
        cx.observe_window_bounds(window, |this, window, _| {
            let size = window.viewport_size();
            this.state.window = Some(WindowSize { width: f32::from(size.width) as u32, height: f32::from(size.height) as u32 });
            this.state_dirty = true;
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_TICK).await;
                if this.update(cx, |this, cx| this.on_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        app
    }

    /// Opens `path`, or switches to its Tab when it is already open.
    fn open(&mut self, path: &Path, format: Option<Format>) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if let Some(i) = self.tabs.iter().position(|t| t.file.path() == path) {
            self.active = i;
            return;
        }
        match LogFile::open(&path, format) {
            Ok(file) => {
                self.tabs.push(OpenTab::new(file));
                self.active = self.tabs.len() - 1;
                self.message = None;
                self.remember_recent(path);
            }
            Err(e) => self.message = Some(format!("Cannot open {}: {e}", path.display())),
        }
    }

    fn remember_recent(&mut self, path: PathBuf) {
        self.state.recent_files.retain(|p| *p != path);
        self.state.recent_files.insert(0, path);
        self.state.recent_files.truncate(MAX_RECENT);
        self.state_dirty = true;
    }

    fn close(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.tabs.remove(index);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    fn on_tick(&mut self, cx: &mut Context<Self>) {
        self.tick += 1;
        let background_turn = self.tick.is_multiple_of(BACKGROUND_EVERY);
        let mut changed = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i != self.active && !background_turn {
                continue;
            }
            match tab.file.poll() {
                Ok(Polled::Unchanged) => {}
                Ok(_) => changed = true,
                Err(e) => self.message = Some(format!("{}: {e}", tab.title())),
            }
        }
        // The first indexing pass reports progress without any file change.
        if self.tabs.get(self.active).is_some_and(|t| !t.file.is_indexed()) {
            changed = true;
        }
        if self.state_dirty {
            self.state_dirty = false;
            if let Some(Err(e)) = settings_path(settings::STATE_FILE).map(|p| settings::save(&p, &self.state)) {
                self.message = Some(e.to_string());
            }
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update(cx, |this, cx| {
                    for path in paths {
                        this.open(&path, None);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_format(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let next = match tab.file.format() {
            Format::PlainText => Format::JsonLines,
            Format::JsonLines => Format::PlainText,
        };
        match LogFile::open(tab.file.path(), Some(next)) {
            Ok(file) => *tab = OpenTab::new(file),
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let recent: Vec<_> = self
            .state
            .recent_files
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let target = path.clone();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                h_flex()
                    .id(("recent", i))
                    .gap_3()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .child(name)
                    .child(div().text_color(theme.muted_foreground).child(dir))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open(&target, None);
                        cx.notify();
                    }))
            })
            .collect();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(div().text_xl().child("Glance"))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Button::new("open").primary().label("Open File…").on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))))
                    .child(div().text_color(theme.muted_foreground).child("or drop a file here")),
            )
            .when(!recent.is_empty(), |s| {
                s.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_color(theme.muted_foreground).child("Recent files"))
                        .children(recent),
                )
            })
    }

    fn render_tab_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tab = &self.tabs[self.active];
        let entries = tab.file.index().read().unwrap().len();
        let rotation_bar = tab.file.rotated().map(|rotation| {
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
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .child(format!("⚠ This file {what}. Following has stopped."))
                .child(Button::new("reload").small().label("Reload").on_click(cx.listener(|this, _, _, cx| {
                    if let Some(Err(e)) = this.tabs.get_mut(this.active).map(|t| t.file.reload()) {
                        this.message = Some(format!("Cannot reload: {e}"));
                    }
                    cx.notify();
                })))
        });
        v_flex()
            .size_full()
            .children(rotation_bar)
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_color(theme.muted_foreground)
                    .child("Filter: (coming in a later step)")
                    .child("Search: (coming in a later step)"),
            )
            .child(self.render_entry_view(entries, cx))
    }

    fn row_height(&self) -> f32 {
        (self.font_size() * 1.5).ceil()
    }

    fn font_size(&self) -> f32 {
        self.config.font_size.filter(|s| *s >= 6.0).unwrap_or(DEFAULT_FONT_SIZE)
    }

    fn render_entry_view(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = &self.tabs[self.active];
        let index = tab.file.index().clone();
        let reader = tab.file.reader().clone();
        let selection = tab.selection.clone();
        let scroll_x = tab.scroll_x;
        let max_chars = tab.max_chars.clone();
        let family: SharedString = self.config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let font_size = self.font_size();
        let row_height = self.row_height();
        let (muted, selected_bg, hover_bg) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.list_active, theme.list_hover)
        };
        let list = uniform_list(
            "entries",
            count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                let index = index.read().unwrap();
                range
                    .map(|row| {
                        // Until Filter is wired in, a row is an Entry.
                        let summary = entry::read_summary(&index, &reader, row).ok().flatten();
                        let (text, extra, more) = summary
                            .map_or((String::new(), 0, false), |s| (s.first_line, s.extra_lines, s.more));
                        max_chars.set(max_chars.get().max(text.chars().count() + 12));
                        let badge = (extra > 0).then(|| format!("  [+{extra}{} lines]", if more { "+" } else { "" }));
                        h_flex()
                            .id(("row", row))
                            .h(px(row_height))
                            .w_full()
                            .flex_none()
                            .when(selection.contains(row), |s| s.bg(selected_bg))
                            .hover(|s| s.bg(hover_bg))
                            .child(
                                div()
                                    .w(px(GUTTER_WIDTH))
                                    .flex_none()
                                    .pr_3()
                                    .text_right()
                                    .text_color(muted)
                                    .child((row + 1).to_string()),
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
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();
        div()
            .id("entry-view")
            .flex_1()
            .min_h_0()
            .font_family(family)
            .text_size(px(font_size))
            .key_context("EntryView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(row_height));
                let dx = f32::from(delta.x);
                if dx != 0.0 {
                    this.scroll_horizontally(dx, font_size);
                    cx.notify();
                }
            }))
            .child(list)
    }

    fn select_row(&mut self, row: usize, modifiers: Modifiers) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if modifiers.shift {
            tab.selection.extend_to(row);
        } else if modifiers.secondary() {
            tab.selection.toggle(row);
        } else {
            tab.selection.click(row);
        }
    }

    fn scroll_horizontally(&mut self, dx: f32, font_size: f32) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        // Monospace glyphs are about 0.6 em wide.
        let content = tab.max_chars.get() as f32 * font_size * 0.6;
        tab.scroll_x = (tab.scroll_x - dx).clamp(0.0, (content - 200.0).max(0.0));
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        if tab.selection.is_empty() {
            return;
        }
        let index = tab.file.index().read().unwrap();
        // Until Filter is wired in, a row is an Entry.
        match entry::copy_entries(&index, tab.file.reader(), tab.selection.rows(), COPY_LIMIT) {
            Ok(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Err(e) => self.message = Some(e.to_string()),
        }
        cx.notify();
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bar = h_flex().px_3().py_1().gap_4().border_t_1().border_color(theme.border).text_sm().text_color(theme.muted_foreground);
        let Some(tab) = self.tabs.get(self.active) else {
            return bar.child(self.message.clone().unwrap_or_default());
        };
        let (entries, scanned) = {
            let index = tab.file.index().read().unwrap();
            (index.len(), index.scanned())
        };
        let total = tab.file.reader().len().unwrap_or(0);
        let progress = if tab.file.is_indexed() || total == 0 {
            "indexed".to_owned()
        } else {
            format!("indexing {}%", scanned * 100 / total)
        };
        bar.child("UTF-8")
            .child(
                div()
                    .id("format")
                    .cursor_pointer()
                    .child(format!("{} ▾", tab.file.format().as_str()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_format();
                        cx.notify();
                    })),
            )
            .child(format!("{entries} entries"))
            .child(progress)
            .children(self.message.clone())
    }
}

impl Render for GlanceApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let warn = if tab.file.rotated().is_some() { "⚠ " } else { "" };
                Tab::new().label(format!("{warn}{}", tab.title())).suffix(
                    Button::new(("close", i)).ghost().xsmall().label("×").on_click(cx.listener(move |this, _, _, cx| {
                        this.close(i);
                        cx.notify();
                    })),
                )
            })
            .collect();
        let tab_bar = TabBar::new("tabs")
            .selected_index(self.active)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.active = *ix;
                cx.notify();
            }))
            .children(tabs)
            .suffix(Button::new("add").ghost().small().label("+").on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))));
        let body = if self.tabs.is_empty() {
            self.render_welcome(cx).into_any_element()
        } else {
            self.render_tab_content(cx).into_any_element()
        };
        v_flex()
            .size_full()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                for path in paths.paths() {
                    this.open(path, None);
                }
                cx.notify();
            }))
            .child(tab_bar)
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
