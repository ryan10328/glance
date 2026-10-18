//! Window shell: Tab bar, status bar and the open-file flows.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use glance_core::format::Format;
use glance_core::highlight::Highlighter;
use glance_core::session::{LogFile, Polled};
use glance_core::settings::{self, Config, State, WindowSize};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::highlight_bar::{HighlightBar, HighlightBarEvent};
use crate::tab_view::TabView;

const POLL_TICK: Duration = Duration::from_millis(500);
/// Background Tabs are polled every this many ticks (about 2 s).
const BACKGROUND_EVERY: u64 = 4;
const MAX_RECENT: usize = 20;

pub struct GlanceApp {
    tabs: Vec<Entity<TabView>>,
    active: usize,
    config: Rc<Config>,
    highlight_bar: Entity<HighlightBar>,
    highlighter: Rc<Highlighter>,
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
        let highlight_bar = cx.new(|cx| HighlightBar::new(&state.highlight_rules, window, cx));
        cx.subscribe(&highlight_bar, |this, _, event: &HighlightBarEvent, cx| {
            let HighlightBarEvent::Changed { rules, highlighter } = event;
            this.state.highlight_rules = rules.clone();
            this.state_dirty = true;
            this.highlighter = highlighter.clone();
            for tab in &this.tabs {
                tab.update(cx, |tab, cx| tab.set_highlighter(highlighter.clone(), cx));
            }
            cx.notify();
        })
        .detach();
        let highlighter = Rc::new(Highlighter::new(&state.highlight_rules).0);
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            config: Rc::new(config),
            highlight_bar,
            highlighter,
            state,
            state_dirty: false,
            message,
            tick: 0,
        };
        for path in args {
            app.open(&path, None, window, cx);
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
    fn open(&mut self, path: &Path, format: Option<Format>, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if let Some(i) = self.tabs.iter().position(|t| t.read(cx).path() == path) {
            self.active = i;
            return;
        }
        match LogFile::open(&path, format) {
            Ok(file) => {
                self.tabs.push(TabView::new(file, self.config.clone(), self.highlighter.clone(), self.highlight_bar.clone(), window, cx));
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
        for (i, tab) in self.tabs.iter().enumerate() {
            if i != self.active && !background_turn {
                continue;
            }
            match tab.update(cx, |tab, cx| tab.poll(cx)) {
                Ok(Polled::Unchanged) => {}
                Ok(_) => changed = true,
                Err(e) => self.message = Some(format!("{}: {e}", tab.read(cx).title())),
            }
        }
        // The first indexing pass reports progress without any file change.
        if self.tabs.get(self.active).is_some_and(|t| !t.read(cx).file().is_indexed()) {
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

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.open(&path, None, window, cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn toggle_format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (path, format) = {
            let tab = tab.read(cx);
            (tab.path().to_owned(), tab.file().format())
        };
        let next = match format {
            Format::PlainText => Format::JsonLines,
            Format::JsonLines => Format::PlainText,
        };
        match LogFile::open(&path, Some(next)) {
            Ok(file) => self.tabs[self.active] = TabView::new(file, self.config.clone(), self.highlighter.clone(), self.highlight_bar.clone(), window, cx),
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(&target, None, window, cx);
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
                    .child(Button::new("open").primary().label("Open File…")
.on_click(cx.listener(|this, _, window, cx| this.prompt_open(window, cx))))
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bar = h_flex().px_3().py_1().gap_4().border_t_1().border_color(theme.border).text_sm().text_color(theme.muted_foreground);
        let Some(tab) = self.tabs.get(self.active) else {
            return bar.child(self.message.clone().unwrap_or_default());
        };
        let tab = tab.read(cx);
        let (shown, total) = tab.counts();
        let (scanned, indexed) = (tab.file().index().read().unwrap().scanned(), tab.file().is_indexed());
        let size = tab.file().reader().len().unwrap_or(0);
        let progress = if indexed || size == 0 { "indexed".to_owned() } else { format!("indexing {}%", scanned * 100 / size) };
        let entries = if shown == total { format!("{total} entries") } else { format!("{shown} of {total} entries") };
        let message = tab.message().map(str::to_owned).or_else(|| self.message.clone());
        bar.child("UTF-8")
            .child(
                div()
                    .id("format")
                    .cursor_pointer()
                    .child(format!("{} ▾", tab.file().format().as_str()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_format(window, cx);
                        cx.notify();
                    })),
            )
            .child(entries)
            .child(progress)
            .children(message)
    }
}

impl Render for GlanceApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let tab = tab.read(cx);
                let warn = if tab.file().rotated().is_some() { "⚠ " } else { "" };
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
            .suffix(Button::new("add").ghost().small().label("+").on_click(cx.listener(|this, _, window, cx| this.prompt_open(window, cx))));
        let body = if self.tabs.is_empty() {
            self.render_welcome(cx).into_any_element()
        } else {
            self.tabs[self.active].clone().into_any_element()
        };
        v_flex()
            .size_full()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                for path in paths.paths() {
                    this.open(path, None, window, cx);
                }
                cx.notify();
            }))
            .child(tab_bar)
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
