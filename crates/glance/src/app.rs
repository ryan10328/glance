//! Window shell: Tab bar, status bar and the open-file flows.

use std::path::{Path, PathBuf};
use std::time::Duration;

use glance_core::format::Format;
use glance_core::session::{LogFile, Polled};
use glance_core::settings::{self, Config, State, WindowSize};
use glance_core::watch::Rotation;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

const POLL_TICK: Duration = Duration::from_millis(500);
/// Background Tabs are polled every this many ticks (about 2 s).
const BACKGROUND_EVERY: u64 = 4;
const MAX_RECENT: usize = 20;

struct OpenTab {
    file: LogFile,
}

impl OpenTab {
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
        let mut app = Self { tabs: Vec::new(), active: 0, config, state, state_dirty: false, message, tick: 0 };
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
                self.tabs.push(OpenTab { file });
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
            Ok(file) => tab.file = file,
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
            .child(
                div()
                    .flex_1()
                    .p_3()
                    .child(format!("{entries} entries (Entry view comes in a later step)")),
            )
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
