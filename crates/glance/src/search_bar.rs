//! The Search controls: query box, `Aa` / `.*` toggles, match count and next/previous.

use glance_core::filter::TextPattern;
use glance_core::search::SearchQuery;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::*;

pub enum SearchBarEvent {
    /// The query text or an option changed; Search runs live.
    Changed(SearchQuery),
    Next,
    Previous,
    Close,
}

impl EventEmitter<SearchBarEvent> for SearchBar {}

pub struct SearchBar {
    input: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    /// "3 / 27", or empty when there is no query.
    status: SharedString,
    invalid: bool,
}

impl SearchBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Change => this.changed(cx),
            InputEvent::PressEnter { shift, .. } => {
                cx.emit(if *shift { SearchBarEvent::Previous } else { SearchBarEvent::Next });
            }
            _ => {}
        })
        .detach();
        Self { input, case_sensitive: false, regex: false, status: SharedString::default(), invalid: false }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn query(&self, cx: &App) -> SearchQuery {
        let text = self.input.read(cx).value().to_string();
        let pattern = if self.regex { TextPattern::Regex(text) } else { TextPattern::Wildcard(text) };
        SearchQuery { pattern, case_sensitive: self.case_sensitive }
    }

    pub fn set_status(&mut self, status: String, invalid: bool, cx: &mut Context<Self>) {
        let status: SharedString = status.into();
        if self.status != status || self.invalid != invalid {
            self.status = status;
            self.invalid = invalid;
            cx.notify();
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        cx.emit(SearchBarEvent::Changed(query));
    }
}

fn flag(id: &'static str, label: &'static str, tooltip: &'static str, on: bool) -> Button {
    let button = Button::new(id).xsmall().label(label).tooltip(tooltip);
    if on { button.primary() } else { button.ghost() }
}

impl Render for SearchBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
        h_flex()
            .gap_1()
            .items_center()
            .key_context("SearchBar")
            .child(div().w(px(48.)).text_color(muted).child("Search"))
            .child(div().w(px(280.)).child(Input::new(&self.input).small()))
            .child(flag("search-aa", "Aa Match case", "Case-sensitive matching", self.case_sensitive).on_click(cx.listener(|this, _, _, cx| {
                this.case_sensitive ^= true;
                this.changed(cx);
            })))
            .child(flag("search-re", ".* Regex", "Off: * and ? are wildcards. On: regular expression", self.regex).on_click(cx.listener(|this, _, _, cx| {
                this.regex ^= true;
                this.changed(cx);
            })))
            .child(div().min_w(px(72.)).text_color(if self.invalid { danger } else { muted }).child(self.status.clone()))
            .child(Button::new("search-prev").xsmall().ghost().label("↑ Prev").tooltip("Previous match (Shift+Enter)").on_click(cx.listener(|_, _, _, cx| cx.emit(SearchBarEvent::Previous))))
            .child(Button::new("search-next").xsmall().ghost().label("↓ Next").tooltip("Next match (Enter)").on_click(cx.listener(|_, _, _, cx| cx.emit(SearchBarEvent::Next))))
            .child(Button::new("search-close").xsmall().ghost().label("× Close").tooltip("Close (Esc)").on_click(cx.listener(|_, _, _, cx| cx.emit(SearchBarEvent::Close))))
    }
}
