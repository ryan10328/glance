//! The Settings window: theme, Entry view font and size, and Filter context.
//! Every change applies at once and is saved by the app.

use std::path::PathBuf;
use std::rc::Rc;

use glance_core::filter::MAX_CONTEXT;
use glance_core::settings::{self, Config, ThemeChoice};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::app::GlanceApp;
use crate::tab_view::{DEFAULT_FONT, DEFAULT_FONT_SIZE};

const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;
const PREVIEW: &str = "2026-10-18 19:50:03.412 ERROR request 42 failed: timeout";

pub struct SettingsView {
    app: Entity<GlanceApp>,
    /// Copies of the app's settings, refreshed when it changes: the app cannot be read while it opens this window.
    config: Rc<Config>,
    theme: ThemeChoice,
    font: Entity<SelectState<SearchableVec<String>>>,
    size: Entity<InputState>,
    context: Entity<InputState>,
}

impl SettingsView {
    /// `config` and `theme` are passed in because the app is being updated while this window opens.
    pub fn new(app: Entity<GlanceApp>, config: Rc<Config>, theme: ThemeChoice, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut fonts = cx.text_system().all_font_names();
        fonts.sort_unstable_by_key(|name| name.to_lowercase());
        fonts.dedup();
        let selected = config.font_family.as_ref().and_then(|f| fonts.iter().position(|n| n == f)).map(IndexPath::new);
        let font = cx.new(|cx| SelectState::new(SearchableVec::new(fonts.clone()), selected, window, cx).searchable(true));
        cx.subscribe(&font, |this, _, event: &SelectEvent<SearchableVec<String>>, cx| {
            let SelectEvent::Confirm(name) = event;
            this.set_font(name.clone(), cx);
        })
        .detach();

        let size_value = config.font_size.unwrap_or(DEFAULT_FONT_SIZE);
        let size = cx.new(|cx| InputState::new(window, cx).default_value(format_number(size_value)));
        cx.subscribe_in(&size, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change)
                && let Ok(value) = input.read(cx).value().trim().parse::<f32>()
            {
                this.set_font_size(value, cx);
            }
        })
        .detach();
        cx.subscribe_in(&size, window, |this, input, NumberInputEvent::Step(action): &NumberInputEvent, window, cx| {
            let current = this.config.font_size.unwrap_or(DEFAULT_FONT_SIZE);
            let next = (if *action == StepAction::Increment { current + 1.0 } else { current - 1.0 }).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
            input.update(cx, |input, cx| input.set_value(format_number(next), window, cx));
        })
        .detach();

        let context = cx.new(|cx| InputState::new(window, cx).default_value(config.filter_context.to_string()));
        cx.subscribe_in(&context, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change)
                && let Ok(value) = input.read(cx).value().trim().parse::<usize>()
            {
                this.set_context(value, cx);
            }
        })
        .detach();
        cx.subscribe_in(&context, window, |this, input, NumberInputEvent::Step(action): &NumberInputEvent, window, cx| {
            let current = this.config.filter_context;
            let next = if *action == StepAction::Increment { (current + 1).min(MAX_CONTEXT) } else { current.saturating_sub(1) };
            input.update(cx, |input, cx| input.set_value(next.to_string(), window, cx));
        })
        .detach();

        // The theme can also change from the Tab bar.
        cx.observe(&app, |this, app, cx| {
            let app = app.read(cx);
            this.config = app.config().clone();
            this.theme = app.theme_choice();
            cx.notify();
        })
        .detach();
        Self { app, config, theme, font, size, context }
    }

    fn set_font(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| app.update_config(|c| c.font_family = name, cx));
    }

    fn set_font_size(&mut self, value: f32, cx: &mut Context<Self>) {
        let value = value.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.app.update(cx, |app, cx| app.update_config(|c| c.font_size = Some(value), cx));
    }

    fn set_context(&mut self, value: usize, cx: &mut Context<Self>) {
        let value = value.min(MAX_CONTEXT);
        self.app.update(cx, |app, cx| app.update_config(|c| c.filter_context = value, cx));
    }

    fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.app.update(cx, |app, cx| app.update_config(|c| c.wrap_long_lines = wrap, cx));
    }

    fn reset_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.font.update(cx, |font, cx| font.set_selected_index(None, window, cx));
        self.set_font(None, cx);
    }

    fn reset_font_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.size.update(cx, |input, cx| input.set_value(format_number(DEFAULT_FONT_SIZE), window, cx));
        self.app.update(cx, |app, cx| app.update_config(|c| c.font_size = None, cx));
    }

    fn set_theme(&mut self, choice: ThemeChoice, cx: &mut Context<Self>) {
        let app = self.app.clone();
        let main_window = app.read(cx).main_window();
        // The theme needs the main window (to follow the system appearance), not this one.
        let _ = main_window.update(cx, |_, window, cx| app.update(cx, |app, cx| app.set_theme(choice, window, cx)));
    }

    fn row(label: &'static str, control: impl IntoElement, muted: Hsla) -> Div {
        h_flex().gap_3().items_center().child(div().w(px(110.)).text_color(muted).child(label)).child(control)
    }

    fn section(title: &'static str, border: Hsla) -> Div {
        v_flex().gap_2().pb_3().child(div().pb_1().border_b_1().border_color(border).font_weight(FontWeight::SEMIBOLD).child(title))
    }
}

fn format_number(value: f32) -> String {
    if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, border, background, foreground) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.background, theme.foreground)
        };
        let (config, theme_choice) = (self.config.clone(), self.theme);
        let family: SharedString = config.font_family.clone().unwrap_or_else(|| DEFAULT_FONT.to_owned()).into();
        let size = config.font_size.unwrap_or(DEFAULT_FONT_SIZE).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        let theme_label = match theme_choice {
            ThemeChoice::System => "◐ System",
            ThemeChoice::Light => "☀ Light",
            ThemeChoice::Dark => "☾ Dark",
        };
        let this = cx.entity();
        let theme_menu = Button::new("settings-theme").small().outline().label(format!("{theme_label} ▾")).dropdown_menu(move |menu, _, _| {
            [("System", ThemeChoice::System), ("Light", ThemeChoice::Light), ("Dark", ThemeChoice::Dark)]
                .into_iter()
                .fold(menu, |menu, (name, choice)| {
                    let this = this.clone();
                    menu.item(PopupMenuItem::new(name).checked(theme_choice == choice).on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| this.set_theme(choice, cx));
                    }))
                })
        });
        let font_row = h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(260.)).child(
                Select::new(&self.font).placeholder(format!("{DEFAULT_FONT} (default)")).search_placeholder("Search fonts").menu_max_h(px(320.)),
            ))
            .child(Button::new("font-reset").xsmall().ghost().label("Reset").tooltip("Use the default font").on_click(cx.listener(|this, _, window, cx| this.reset_font(window, cx))));
        let size_row = h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(130.)).child(NumberInput::new(&self.size).small()))
            .child(div().text_color(muted).child("px"))
            .child(Button::new("size-reset").xsmall().ghost().label("Reset").tooltip("Use the default size").on_click(cx.listener(|this, _, window, cx| this.reset_font_size(window, cx))));
        let context_row = h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(130.)).child(NumberInput::new(&self.context).small()))
            .child(div().text_color(muted).child(format!("entries around each match (0 to {MAX_CONTEXT}); applies on the next Apply")));
        let path: Option<PathBuf> = settings::settings_dir().map(|dir| dir.join(settings::CONFIG_FILE));
        let shown = path.as_ref().map_or_else(|| "unavailable".to_owned(), |p| p.display().to_string());
        let reveal = path.clone();
        v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .p_5()
            .gap_4()
            .child(
                Self::section("Appearance", border).child(Self::row("Theme", theme_menu, muted)),
            )
            .child(
                Self::section("Entry view", border)
                    .child(Self::row("Font", font_row, muted))
                    .child(Self::row("Font size", size_row, muted))
                    .child(Self::row(
                        "Preview",
                        div()
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(border)
                            .rounded(px(4.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .font_family(family)
                            .text_size(px(size))
                            .child(PREVIEW),
                        muted,
                    )),
            )
            .child(Self::section("Detail panel", border).child(Self::row(
                "Long lines",
                {
                    let this = cx.entity();
                    Checkbox::new("wrap-lines").small().checked(config.wrap_long_lines).child("Wrap long lines").on_click(move |checked, _, cx| {
                        let checked = *checked;
                        this.update(cx, |this, cx| this.set_wrap(checked, cx));
                    })
                },
                muted,
            )))
            .child(Self::section("Filter", border).child(Self::row("Context", context_row, muted)))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(div().child(format!("Settings file: {shown}")))
                    .child(Button::new("reveal").xsmall().outline().label("Show").tooltip("Show the settings folder").on_click(
                        move |_, _, cx| {
                            if let Some(path) = reveal.as_ref() {
                                match path.exists() {
                                    true => cx.reveal_path(path),
                                    false => {
                                        if let Some(dir) = path.parent() {
                                            let _ = std::fs::create_dir_all(dir);
                                            cx.reveal_path(dir);
                                        }
                                    }
                                }
                            }
                        },
                    )),
            )
    }
}
