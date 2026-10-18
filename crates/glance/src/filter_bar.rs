//! The Filter controls: Level toggles and text conditions.

use glance_core::filter::{
    Condition, Filter, LevelFilter, TextCondition, TextPattern, MAX_CONTEXT,
};
use glance_core::level::Level;

use crate::level_style::LevelPalette;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
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
    context: Entity<InputState>,
    error: Option<String>,
    summary: SharedString,
}

impl FilterBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let context = cx.new(|cx| InputState::new(window, cx).placeholder("0").default_value("0"));
        Self::apply_on_enter(&context, window, cx);
        let mut bar = Self {
            levels: LevelFilter::default(),
            levels_before: None,
            texts: Vec::new(),
            context,
            error: None,
            summary: SharedString::default(),
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
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.apply(cx);
            }
        })
        .detach();
    }

    pub fn set_summary(&mut self, summary: String, cx: &mut Context<Self>) {
        let summary: SharedString = summary.into();
        if self.summary != summary {
            self.summary = summary;
            cx.notify();
        }
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
        let context_text = self.context.read(cx).value();
        let context = match context_text.trim() {
            "" => 0,
            text => text.parse::<usize>().map_err(|_| format!("Context must be a number from 0 to {MAX_CONTEXT}"))?,
        };
        Ok(Filter {
            levels: self.levels.clone(),
            conditions,
            context: context.min(MAX_CONTEXT),
        })
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        match self.build(cx) {
            Ok(filter) => {
                self.error = None;
                cx.emit(FilterBarEvent::Apply(filter));
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// Reports a problem found while compiling the Filter (e.g. a bad regex).
    pub fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.levels = LevelFilter::default();
        self.levels_before = None;
        self.context.update(cx, |i, cx| i.set_value("0", window, cx));
        self.texts.clear();
        self.add_text_row(window, cx);
        self.error = None;
        cx.emit(FilterBarEvent::Apply(Filter::default()));
        cx.notify();
    }

    fn level_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let palette = LevelPalette::new(cx);
        let check = |id: String, label: &'static str, level: Option<Level>, tooltip: String, on: bool, f: Box<dyn Fn(&mut LevelFilter)>| {
            let entity = entity.clone();
            let style = palette.style(level);
            Checkbox::new(SharedString::from(id))
                .small()
                .accessibility_label(label)
                .child(
                    // The label wears the Level's color from the Entry view; a hidden Level fades.
                    div()
                        .px_1()
                        .rounded_sm()
                        .when(!on, |s| s.opacity(0.45))
                        .when_some(style.tint, |s, tint| s.bg(tint))
                        .when(style.faded, |s| s.text_color(palette.muted()))
                        .child(label),
                )
                .tooltip(tooltip)
                .checked(on)
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        f(&mut this.levels);
                        this.levels_before = None;
                        this.apply(cx);
                    });
                })
        };
        let mut row = h_flex().gap_3().items_center().child(div().w(px(48.)).text_color(cx.theme().muted_foreground).child("Level"));
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
        let all = self.levels == LevelFilter::default();
        let warn_on = self.levels == warn_up;
        row.child(
            flag("lv-all".into(), "All", "Show every level, including entries without one. Click again to switch back.", all).on_click(
                cx.listener(|this, _, _, cx| {
                    // Without an earlier selection to go back to, switching off shows no level at all.
                    this.toggle_preset(LevelFilter::default(), LevelFilter::only(&[], false), cx);
                }),
            ),
        )
        .child(
            flag("lv-warn-up".into(), "WARN and above", "Show WARN, ERROR and FATAL only. Click again to switch back.", warn_on).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.toggle_preset(warn_up.clone(), LevelFilter::default(), cx);
                }),
            ),
        )
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

fn flag(id: SharedString, label: &'static str, tooltip: &'static str, on: bool) -> Button {
    let button = Button::new(id).xsmall().label(label).tooltip(tooltip);
    if on { button.primary() } else { button.ghost() }
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
                    .child(div().w(px(48.)).text_color(muted).child(if i == 0 { "Text" } else { "" }))
                    .child(Self::sized(&row.input, 280.))
                    .child(flag(format!("aa-{i}").into(), "Aa Match case", "Case-sensitive matching", row.case_sensitive).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.texts[i].case_sensitive ^= true;
                            this.apply(cx);
                        }),
                    ))
                    .child(flag(format!("re-{i}").into(), ".* Regex", "Off: * and ? are wildcards. On: regular expression", row.regex).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.texts[i].regex ^= true;
                            this.apply(cx);
                        }),
                    ))
                    .child(flag(format!("not-{i}").into(), "¬ Exclude", "Hide entries that match (NOT)", row.exclude).on_click(
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
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(muted).child("Context"))
                    .child(Self::sized(&self.context, 48.))
                    .child(div().text_color(muted).child("entries around each match"))
                    .child(Button::new("apply").xsmall().primary().label("Apply ⏎").tooltip("Apply the Filter (Enter)").on_click(
                        cx.listener(|this, _, _, cx| this.apply(cx)),
                    ))
                    .child(Button::new("clear").xsmall().outline().label("Clear all").tooltip("Remove every condition").on_click(
                        cx.listener(|this, _, window, cx| this.clear(window, cx)),
                    ))
                    .child(div().text_color(muted).child(self.summary.clone()))
                    .children(self.error.clone().map(|e| div().text_color(danger).child(e))),
            )
    }
}
