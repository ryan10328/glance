//! The Highlight Rules: text to color wherever it appears, applied as you type.

use std::rc::Rc;

use glance_core::highlight::{Highlighter, PALETTE, Problem, palette_rgb};
use glance_core::settings::HighlightRule;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tab_view::color_of;

pub enum HighlightBarEvent {
    /// The rules changed. `rules` leaves out the ones still empty.
    Changed { rules: Vec<HighlightRule>, highlighter: Rc<Highlighter> },
}

impl EventEmitter<HighlightBarEvent> for HighlightBar {}

struct Row {
    input: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    color: usize,
}

pub struct HighlightBar {
    rows: Vec<Row>,
    problems: Vec<Problem>,
}

impl HighlightBar {
    pub fn new(rules: &[HighlightRule], window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut bar = Self { rows: Vec::new(), problems: Vec::new() };
        for rule in rules {
            bar.push_row(&rule.pattern, rule.case_sensitive, rule.regex, rule.color, window, cx);
        }
        bar.problems = Highlighter::new(&bar.rules(cx)).1;
        bar
    }

    fn push_row(&mut self, pattern: &str, case_sensitive: bool, regex: bool, color: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pattern = pattern.to_owned();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("text, * ? wildcards, or regex").default_value(pattern));
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.changed(cx);
            }
        })
        .detach();
        self.rows.push(Row { input, case_sensitive, regex, color });
    }

    fn rules(&self, cx: &App) -> Vec<HighlightRule> {
        self.rows
            .iter()
            .map(|row| HighlightRule {
                pattern: row.input.read(cx).value().to_string(),
                regex: row.regex,
                case_sensitive: row.case_sensitive,
                color: row.color,
            })
            .collect()
    }

    /// Recompiles the rules and tells the window; every edit applies at once.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let all = self.rules(cx);
        let (highlighter, problems) = Highlighter::new(&all);
        self.problems = problems;
        let rules = all.into_iter().filter(|r| !r.pattern.is_empty()).collect();
        cx.emit(HighlightBarEvent::Changed { rules, highlighter: Rc::new(highlighter) });
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Each new rule starts with the next color so rules look different at once.
        let color = self.rows.len() % PALETTE.len();
        self.push_row("", false, false, color, window, cx);
        self.rows.last().unwrap().input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }
}

fn flag(id: SharedString, label: &'static str, tooltip: &'static str, on: bool) -> Button {
    let button = Button::new(id).xsmall().label(label).tooltip(tooltip);
    if on { button.primary() } else { button.ghost() }
}

impl Render for HighlightBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, danger, ring) = (cx.theme().muted_foreground, cx.theme().danger, cx.theme().foreground);
        let dark = cx.theme().is_dark();
        let count = self.rows.len();
        let add = |cx: &mut Context<Self>| {
            Button::new("add-highlight").xsmall().outline().label("+ Add highlight").tooltip("Color some text wherever it appears").on_click(
                cx.listener(|this, _, window, cx| this.add(window, cx)),
            )
        };
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let swatches: Vec<_> = PALETTE
                    .iter()
                    .enumerate()
                    .map(|(c, _)| {
                        let selected = row.color == c;
                        div()
                            .id(("swatch", i * PALETTE.len() + c))
                            .size_4()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(color_of(palette_rgb(c, dark)))
                            .border_2()
                            .border_color(if selected { ring } else { gpui_kit::transparent_black() })
                            .when(!selected, |s| s.opacity(0.7))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rows[i].color = c;
                                this.changed(cx);
                            }))
                    })
                    .collect();
                let problem = self.problems.iter().find(|(n, _)| *n == i).map(|_| "invalid pattern");
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().w(px(72.)).text_color(muted).child(if i == 0 { "Highlight" } else { "" }))
                    .child(div().w(px(240.)).child(Input::new(&row.input).small()))
                    .child(flag(format!("hl-aa-{i}").into(), "Aa Match case", "Case-sensitive matching", row.case_sensitive).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.rows[i].case_sensitive ^= true;
                            this.changed(cx);
                        }),
                    ))
                    .child(flag(format!("hl-re-{i}").into(), ".* Regex", "Off: * and ? are wildcards. On: regular expression", row.regex).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.rows[i].regex ^= true;
                            this.changed(cx);
                        }),
                    ))
                    .child(h_flex().gap_1().px_2().children(swatches))
                    .child(
                        Button::new(SharedString::from(format!("hl-rm-{i}")))
                            .xsmall()
                            .ghost()
                            .label("× Remove")
                            .tooltip("Remove this highlight")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rows.remove(i);
                                this.changed(cx);
                            })),
                    )
                    .when(i + 1 == count, |s| s.child(add(cx)))
                    .children(problem.map(|p| div().text_color(danger).child(p)))
            })
            .collect();
        v_flex().gap_1().children(rows).when(count == 0, |s| {
            s.child(h_flex().gap_1().items_center().child(div().w(px(72.)).text_color(muted).child("Highlight")).child(add(cx)))
        })
    }
}
