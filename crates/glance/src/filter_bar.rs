//! The Filter controls: Level toggles and text conditions.

use glance_core::filter::{
    Condition, Filter, LevelFilter, TextCondition, TextPattern, MAX_CONTEXT,
};
use glance_core::level::Level;

use crate::level_style::LevelPalette;
use crate::toggle::{ToggleColors, toggle};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub enum FilterBarEvent {
    /// The user applied a (possibly empty) Filter.
    Apply(Filter),
}

impl EventEmitter<FilterBarEvent> for FilterBar {}

struct TextRow {
    input: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    exclude: bool,
}

pub struct FilterBar {
    levels: LevelFilter,
    /// The selection before a preset button was switched on, so that button can switch back.
    levels_before: Option<LevelFilter>,
    texts: Vec<TextRow>,
    /// Entries kept around each match; a setting, not a field of the bar.
    context: usize,
    error: Option<String>,
    /// Text was typed and not applied yet.
    dirty: bool,
}

impl FilterBar {
    pub fn new(context: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut bar = Self {
            levels: LevelFilter::default(),
            levels_before: None,
            texts: Vec::new(),
            context: context.min(MAX_CONTEXT),
            error: None,
            dirty: false,
        };
        bar.add_text_row(window, cx);
        bar
    }

    fn input(placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()));
        Self::apply_on_enter(&input, window, cx);
        input
    }

    /// Enter or leaving the field applies the Filter.
    fn apply_on_enter(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => {
                    this.dirty = true;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.apply(cx),
                _ => {}
            }
        })
        .detach();
    }

    fn add_text_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = Self::input("text, * ? wildcards, or regex", window, cx);
        self.texts.push(TextRow { input, case_sensitive: false, regex: false, exclude: false });
    }

    /// The Filter described by the controls, or why it is not valid.
    fn build(&self, cx: &App) -> Result<Filter, String> {
        let mut conditions = Vec::new();
        for row in &self.texts {
            let text = row.input.read(cx).value();
            if text.is_empty() {
                continue;
            }
            let pattern = if row.regex { TextPattern::Regex(text.to_string()) } else { TextPattern::Wildcard(text.to_string()) };
            conditions.push(Condition::Text(TextCondition { pattern, case_sensitive: row.case_sensitive, exclude: row.exclude }));
        }
        Ok(Filter {
            levels: self.levels.clone(),
            conditions,
            context: self.context,
        })
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        match self.build(cx) {
            Ok(filter) => {
                self.error = None;
                self.dirty = false;
                cx.emit(FilterBarEvent::Apply(filter));
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// The Filter context setting changed; it applies with the next Apply.
    pub fn set_context(&mut self, context: usize, _cx: &mut Context<Self>) {
        self.context = context.min(MAX_CONTEXT);
    }

    /// Reports a problem found while compiling the Filter (e.g. a bad regex).
    pub fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.levels = LevelFilter::default();
        self.levels_before = None;
        self.texts.clear();
        self.add_text_row(window, cx);
        self.error = None;
        self.dirty = false;
        cx.emit(FilterBarEvent::Apply(Filter::default()));
        cx.notify();
    }

    fn level_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let palette = LevelPalette::new(cx);
        let theme = cx.theme();
        let (muted_bg, border, fg) = (theme.secondary, theme.foreground.opacity(0.22), theme.foreground);
        let check = |id: String, label: &'static str, level: Option<Level>, tooltip: String, on: bool, f: Box<dyn Fn(&mut LevelFilter)>| {
            let entity = entity.clone();
            let style = palette.style(level);
            // A Level chip is its own switch: filled in the Level's color when shown, an outline when hidden.
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .cursor_pointer()
                .text_sm()
                .when(on, |s| s.border_color(border).bg(style.tint.unwrap_or(muted_bg)).text_color(if style.faded { palette.muted() } else { fg }))
                .when(!on, |s| s.border_color(border.opacity(0.5)).text_color(palette.muted().opacity(0.7)))
                .child(label)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        f(&mut this.levels);
                        this.levels_before = None;
                        this.apply(cx);
                    });
                })
        };
        let mut row = h_flex().gap_1().items_center().child(Self::label("Level", cx.theme().muted_foreground));
        for level in Level::ALL {
            let on = self.levels.accepts(Some(level));
            row = row.child(check(format!("lv-{}", level.as_str()), level.as_str(), Some(level), format!("Show {} entries", level.as_str()), on, Box::new(move |l| flip_level(l, level))));
        }
        let none_on = self.levels.accepts(None);
        row = row.child(check(
            "lv-none".into(),
            "No level",
            None,
            "Entries whose level could not be recognized".into(),
            none_on,
            Box::new(|levels| {
                let allowed: Vec<Level> = Level::ALL.into_iter().filter(|l| levels.accepts(Some(*l))).collect();
                *levels = LevelFilter::only(&allowed, !levels.accepts(None));
            }),
        ));
        let warn_up = LevelFilter::at_least(Level::Warn, true);
        let tc = ToggleColors::new(cx);
        let muted_text = cx.theme().muted_foreground;
        let all = self.levels == LevelFilter::default();
        let warn_on = self.levels == warn_up;
        let segment = |id: &'static str, label: &'static str, tooltip: &'static str, on: bool| {
            div()
                .id(id)
                .px_2()
                .py(px(1.))
                .cursor_pointer()
                .text_sm()
                .when(on, |s| s.bg(tc.on_fill))
                .when(!on, |s| s.text_color(muted_text))
                .child(label)
                .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
        };
        let segmented = h_flex()
            .ml_2()
            .rounded(px(4.))
            .border_1()
            .border_color(tc.off_edge)
            .overflow_hidden()
            .child(
                segment("lv-all", "All", "Show every level, including entries without one. Click again to switch back.", all).on_click(cx.listener(
                    |this, _, _, cx| {
                        // Without an earlier selection to go back to, switching off shows no level at all.
                        this.toggle_preset(LevelFilter::default(), LevelFilter::only(&[], false), cx);
                    },
                )),
            )
            .child(
                div().border_l_1().border_color(tc.off_edge).child(
                    segment("lv-warn-up", "WARN+", "Show WARN, ERROR and FATAL only. Click again to switch back.", warn_on).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.toggle_preset(warn_up.clone(), LevelFilter::default(), cx);
                        },
                    )),
                ),
            );
        row.child(segmented)
            .child(div().flex_1())
            .child({
                // Typed text waits for Apply; everything else applies at once.
                let apply = Button::new("apply").xsmall().label(if self.dirty { "Apply ⏎ ●" } else { "Apply ⏎" }).tooltip(
                    if self.dirty { "The text has changed and is not applied yet (Enter)" } else { "Apply the text conditions (Enter)" },
                );
                let apply = if self.dirty { apply.primary() } else { apply.outline() };
                apply.on_click(cx.listener(|this, _, _, cx| this.apply(cx)))
            })
            .child(Button::new("clear").xsmall().outline().label("Clear all").tooltip("Reset the Levels and remove every condition").on_click(
                cx.listener(|this, _, window, cx| this.clear(window, cx)),
            ))
    }

    /// Switches a preset on, remembering the selection it replaced; switching it
    /// off restores that selection, or `fallback` when there is none.
    fn toggle_preset(&mut self, preset: LevelFilter, fallback: LevelFilter, cx: &mut Context<Self>) {
        if self.levels == preset {
            self.levels = self.levels_before.take().unwrap_or(fallback);
        } else {
            self.levels_before = Some(std::mem::replace(&mut self.levels, preset));
        }
        self.apply(cx);
    }

    fn sized(input: &Entity<InputState>, width: f32) -> impl IntoElement {
        div().w(px(width)).child(Input::new(input).small())
    }
}

/// Shows or hides one Level, leaving the other Levels and "No level" as they are.
fn flip_level(levels: &mut LevelFilter, level: Level) {
    let mut allowed: Vec<Level> = Level::ALL.into_iter().filter(|l| levels.accepts(Some(*l))).collect();
    match allowed.iter().position(|l| *l == level) {
        Some(i) => {
            allowed.remove(i);
        }
        None => allowed.push(level),
    }
    *levels = LevelFilter::only(&allowed, levels.accepts(None));
}

/// Width of the row labels (`Level`, `Text`, `Highlight`), shared so the rows line up.
pub const LABEL_WIDTH: f32 = 64.0;

impl FilterBar {
    /// A row label: small and muted so it does not outweigh the controls it names.
    pub fn label(text: &'static str, color: Hsla) -> Div {
        div().w(px(LABEL_WIDTH)).flex_none().text_sm().text_color(color).child(text)
    }
}

impl Render for FilterBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        let text_count = self.texts.len();
        let text_rows: Vec<_> = self
            .texts
            .iter()
            .enumerate()
            .map(|(i, row)| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Self::label(if i == 0 { "Text" } else { "" }, muted))
                    .child(Self::sized(&row.input, 280.))
                    .child(toggle(SharedString::from(format!("aa-{i}")), "Aa Match case", "Case-sensitive matching", row.case_sensitive, cx).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.texts[i].case_sensitive ^= true;
                            this.apply(cx);
                        }),
                    ))
                    .child(toggle(SharedString::from(format!("re-{i}")), ".* Regex", "Off: * and ? are wildcards. On: regular expression", row.regex, cx).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.texts[i].regex ^= true;
                            this.apply(cx);
                        }),
                    ))
                    .child(toggle(SharedString::from(format!("not-{i}")), "¬ Exclude", "Hide entries that match (NOT)", row.exclude, cx).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.texts[i].exclude ^= true;
                            this.apply(cx);
                        }),
                    ))
                    .when(text_count > 1, |s| {
                        s.child(
                            Button::new(SharedString::from(format!("rm-text-{i}")))
                                .xsmall()
                                .ghost()
                                .label("× Remove")
                                .tooltip("Remove this condition")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.texts.remove(i);
                                    this.apply(cx);
                                })),
                        )
                    })
                    .when(i + 1 == text_count, |s| {
                        s.child(Button::new("add-text").xsmall().outline().label("+ Add text").tooltip("Add another text condition").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.add_text_row(window, cx);
                                cx.notify();
                            }),
                        ))
                    })
            })
            .collect();

        v_flex()
            .gap_1()
            .child(self.level_buttons(cx))
            .children(text_rows)
            .children(self.error.clone().map(|e| div().pl(px(LABEL_WIDTH)).text_color(danger).child(e)))
    }
}
