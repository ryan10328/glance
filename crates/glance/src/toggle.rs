//! The one look for every on/off switch in the toolbar: filled blue when on,
//! an outline when off. Blue is the app's "chosen" color; Level colors
//! (yellow, red) are kept for severity.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Fill and edge colors for a switch that is on, from the theme once.
#[derive(Clone, Copy)]
pub struct ToggleColors {
    pub on_fill: Hsla,
    pub on_edge: Hsla,
    pub off_edge: Hsla,
    pub off_text: Hsla,
    pub hover: Hsla,
}

impl ToggleColors {
    pub fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let fill = if theme.is_dark() { 0.28 } else { 0.2 };
        Self {
            on_fill: theme.info.opacity(fill),
            on_edge: theme.info.opacity(0.8),
            off_edge: theme.foreground.opacity(0.22),
            off_text: theme.muted_foreground,
            hover: theme.list_hover,
        }
    }
}

/// A labeled switch. Chain `.on_click(...)` on the result.
pub fn toggle(id: impl Into<ElementId>, label: &'static str, tooltip: &'static str, on: bool, cx: &App) -> Stateful<Div> {
    let colors = ToggleColors::new(cx);
    div()
        .id(id)
        .px_2()
        .py(px(2.))
        .rounded(px(4.))
        .border_1()
        .cursor_pointer()
        .text_xs()
        .whitespace_nowrap()
        .when(on, |s| s.bg(colors.on_fill).border_color(colors.on_edge))
        .when(!on, |s| s.border_color(colors.off_edge).text_color(colors.off_text).hover(|s| s.bg(colors.hover)))
        .child(label)
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
}
