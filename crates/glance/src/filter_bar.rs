//! The Filter controls: Level toggles, time range, text and field conditions.

use glance_core::filter::{
    Condition, FieldCondition, FieldOp, Filter, LevelFilter, TextCondition, TextPattern, MAX_CONTEXT,
};
use glance_core::format::parse_user_timestamp;
use glance_core::level::Level;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use jiff::tz::TimeZone;

/// Operators of a field condition, in the order the button cycles through them.
const FIELD_OPS: [&str; 8] = ["=", "contains", "regex", ">", ">=", "<", "<=", "exists"];

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

struct FieldRow {
    path: Entity<InputState>,
    op: usize,
    value: Entity<InputState>,
    exclude: bool,
}

pub struct FilterBar {
    levels: LevelFilter,
    time_from: Entity<InputState>,
    time_to: Entity<InputState>,
    time_available: bool,
    texts: Vec<TextRow>,
    fields: Vec<FieldRow>,
    json: bool,
    context: Entity<InputState>,
    zone: TimeZone,
    error: Option<String>,
    summary: SharedString,
}

impl FilterBar {
    pub fn new(json: bool, time_available: bool, zone: TimeZone, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let time_from = Self::input("from, e.g. 2026-10-18 19:50:00", window, cx);
        let time_to = Self::input("to", window, cx);
        let context = cx.new(|cx| InputState::new(window, cx).placeholder("0").default_value("0"));
        Self::apply_on_enter(&context, window, cx);
        let mut bar = Self {
            levels: LevelFilter::default(),
            time_from,
            time_to,
            time_available,
            texts: Vec::new(),
            fields: Vec::new(),
            json,
            context,
            zone,
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
        let input = Self::input("text or regex", window, cx);
        self.texts.push(TextRow { input, case_sensitive: false, regex: false, exclude: false });
    }

    fn add_field_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = Self::input("field, e.g. req.path", window, cx);
        let value = Self::input("value", window, cx);
        self.fields.push(FieldRow { path, op: 0, value, exclude: false });
    }

    /// The Filter described by the controls, or why it is not valid.
    fn build(&self, cx: &App) -> Result<Filter, String> {
        let time = |input: &Entity<InputState>| -> Result<Option<jiff::Timestamp>, String> {
            let text = input.read(cx).value();
            if text.trim().is_empty() {
                return Ok(None);
            }
            parse_user_timestamp(&text, &self.zone)
                .map(Some)
                .ok_or_else(|| format!("Cannot read the time “{text}”. Use e.g. 2026-10-18 19:50:03"))
        };
        let mut conditions = Vec::new();
        for row in &self.texts {
            let text = row.input.read(cx).value();
            if text.is_empty() {
                continue;
            }
            let pattern = if row.regex { TextPattern::Regex(text.to_string()) } else { TextPattern::Literal(text.to_string()) };
            conditions.push(Condition::Text(TextCondition { pattern, case_sensitive: row.case_sensitive, exclude: row.exclude }));
        }
        for row in &self.fields {
            let path = row.path.read(cx).value().trim().to_owned();
            if path.is_empty() {
                continue;
            }
            let value = row.value.read(cx).value().to_string();
            let number = || value.trim().parse::<f64>().map_err(|_| format!("“{value}” is not a number (field {path})"));
            let op = match FIELD_OPS[row.op] {
                "=" => FieldOp::Equals(value.clone()),
                "contains" => FieldOp::Contains(value.clone()),
                "regex" => FieldOp::Regex(value.clone()),
                ">" => FieldOp::Gt(number()?),
                ">=" => FieldOp::Ge(number()?),
                "<" => FieldOp::Lt(number()?),
                "<=" => FieldOp::Le(number()?),
                _ => FieldOp::Exists,
            };
            conditions.push(Condition::Field(FieldCondition { path, op, exclude: row.exclude }));
        }
        let context_text = self.context.read(cx).value();
        let context = match context_text.trim() {
            "" => 0,
            text => text.parse::<usize>().map_err(|_| format!("Context must be a number from 0 to {MAX_CONTEXT}"))?,
        };
        Ok(Filter {
            levels: self.levels.clone(),
            time_from: time(&self.time_from)?,
            time_to: time(&self.time_to)?,
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
        for input in [&self.time_from, &self.time_to] {
            input.update(cx, |i, cx| i.set_value("", window, cx));
        }
        self.context.update(cx, |i, cx| i.set_value("0", window, cx));
        self.texts.clear();
        self.fields.clear();
        self.add_text_row(window, cx);
        self.error = None;
        cx.emit(FilterBarEvent::Apply(Filter::default()));
        cx.notify();
    }

    fn level_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_1().items_center().child(div().w(px(48.)).child("Level"));
        let toggle = |id: String, label: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut LevelFilter, Option<Level>)| {
            let level = Level::ALL.iter().copied().find(|l| l.as_str() == label);
            let button = Button::new(SharedString::from(id)).xsmall().label(label);
            let button = if on { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, _, cx| {
                f(&mut this.levels, level);
                this.apply(cx);
            }))
        };
        for level in Level::ALL {
            let on = self.levels.accepts(Some(level));
            row = row.child(toggle(format!("lv-{}", level.as_str()), level.as_str(), on, cx, |levels, level| {
                if let Some(level) = level {
                    let mut allowed: Vec<Level> = Level::ALL.into_iter().filter(|l| levels.accepts(Some(*l))).collect();
                    match allowed.iter().position(|l| *l == level) {
                        Some(i) => {
                            allowed.remove(i);
                        }
                        None => allowed.push(level),
                    }
                    *levels = LevelFilter::only(&allowed, levels.accepts(None));
                }
            }));
        }
        let none_on = self.levels.accepts(None);
        row = row.child(toggle("lv-none".into(), "none", none_on, cx, |levels, _| {
            let allowed: Vec<Level> = Level::ALL.into_iter().filter(|l| levels.accepts(Some(*l))).collect();
            *levels = LevelFilter::only(&allowed, !levels.accepts(None));
        }));
        row.child(Button::new("lv-warn-up").xsmall().ghost().label("≥ WARN").on_click(cx.listener(|this, _, _, cx| {
            this.levels = LevelFilter::at_least(Level::Warn, true);
            this.apply(cx);
        })))
    }

    fn sized(input: &Entity<InputState>, width: f32) -> impl IntoElement {
        div().w(px(width)).child(Input::new(input).small())
    }
}

fn flag(id: SharedString, label: &'static str, on: bool) -> Button {
    let button = Button::new(id).xsmall().label(label);
    if on { button.primary() } else { button.ghost() }
}

impl Render for FilterBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let time_available = self.time_available;

        let time_row = h_flex()
            .gap_2()
            .items_center()
            .child(div().text_color(muted).child("Time"))
            .child(Self::sized(&self.time_from, 190.))
            .child("–")
            .child(Self::sized(&self.time_to, 190.))
            .when(!time_available, |s| s.opacity(0.4).child("not available for this file (no full timestamps)"));

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
                    .child(flag(format!("aa-{i}").into(), "Aa", row.case_sensitive).on_click(cx.listener(move |this, _, _, cx| {
                        this.texts[i].case_sensitive ^= true;
                        this.apply(cx);
                    })))
                    .child(flag(format!("re-{i}").into(), ".*", row.regex).on_click(cx.listener(move |this, _, _, cx| {
                        this.texts[i].regex ^= true;
                        this.apply(cx);
                    })))
                    .child(flag(format!("not-{i}").into(), "¬", row.exclude).on_click(cx.listener(move |this, _, _, cx| {
                        this.texts[i].exclude ^= true;
                        this.apply(cx);
                    })))
                    .when(self.texts.len() > 1, |s| {
                        s.child(Button::new(SharedString::from(format!("rm-text-{i}"))).xsmall().ghost().label("×").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.texts.remove(i);
                                this.apply(cx);
                            }),
                        ))
                    })
            })
            .collect();

        let field_rows: Vec<_> = self
            .fields
            .iter()
            .enumerate()
            .map(|(i, row)| {
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().w(px(48.)).text_color(muted).child(if i == 0 { "Field" } else { "" }))
                    .child(Self::sized(&row.path, 160.))
                    .child(
                        Button::new(SharedString::from(format!("op-{i}"))).xsmall().outline().label(FIELD_OPS[row.op]).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.fields[i].op = (this.fields[i].op + 1) % FIELD_OPS.len();
                                this.apply(cx);
                            }),
                        ),
                    )
                    .child(Self::sized(&row.value, 160.))
                    .child(flag(format!("fnot-{i}").into(), "¬", row.exclude).on_click(cx.listener(move |this, _, _, cx| {
                        this.fields[i].exclude ^= true;
                        this.apply(cx);
                    })))
                    .child(Button::new(SharedString::from(format!("rm-field-{i}"))).xsmall().ghost().label("×").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.fields.remove(i);
                            this.apply(cx);
                        },
                    )))
            })
            .collect();

        v_flex()
            .gap_1()
            .child(self.level_buttons(cx))
            .child(time_row)
            .children(text_rows)
            .children(field_rows)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Button::new("add-text").xsmall().ghost().label("+ Text").on_click(cx.listener(|this, _, window, cx| {
                        this.add_text_row(window, cx);
                        cx.notify();
                    })))
                    .when(self.json, |s| {
                        s.child(Button::new("add-field").xsmall().ghost().label("+ Field").on_click(cx.listener(|this, _, window, cx| {
                            this.add_field_row(window, cx);
                            cx.notify();
                        })))
                    })
                    .child(div().text_color(muted).child("Context"))
                    .child(Self::sized(&self.context, 48.))
                    .child(Button::new("apply").xsmall().primary().label("Apply ⏎").on_click(cx.listener(|this, _, _, cx| this.apply(cx))))
                    .child(Button::new("clear").xsmall().outline().label("Clear").on_click(cx.listener(|this, _, window, cx| this.clear(window, cx))))
                    .child(div().text_color(muted).child(self.summary.clone()))
                    .children(self.error.clone().map(|e| div().text_color(danger).child(e))),
            )
    }
}
