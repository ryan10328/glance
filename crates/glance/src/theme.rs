//! A softer dark theme: the built-in one is near black, which leaves little
//! room for the Level colors to show.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig};
use gpui_kit::App;
use serde_json::Value;

/// The built-in dark colors and what replaces them: a dark slate instead of
/// near black, with the chrome a step darker and borders a step lighter.
const SOFTER: [(&str, &str); 9] = [
    ("neutral-950", "#23262d"),
    ("#0a0a0a", "#23262d"),
    ("#171717", "#1c1e23"),
    ("neutral-800", "#343842"),
    ("#262626", "#3a3f4a"),
    ("#212121", "#2f333d"),
    ("#292929", "#3c414d"),
    ("#2f2f2f", "#424752"),
    ("neutral-900", "#2a2d35"),
];

/// Replaces a color that starts with one of the built-in values, keeping any
/// alpha that follows it (`#17171766` or `neutral-800/60`).
fn soften(color: &str) -> Option<String> {
    let (from, to) = SOFTER.iter().find(|(from, _)| color.starts_with(from))?;
    let rest = &color[from.len()..];
    match rest {
        "" => Some((*to).to_owned()),
        _ if rest.starts_with('/') => {
            let percent: f32 = rest[1..].parse().ok()?;
            Some(format!("{to}{:02x}", (percent * 2.55).round() as u8))
        }
        _ if from.starts_with('#') && rest.len() == 2 => Some(format!("{to}{rest}")),
        _ => None,
    }
}

fn soften_all(value: &mut Value) {
    match value {
        Value::String(color) => {
            if let Some(softer) = soften(color) {
                *color = softer;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(soften_all),
        Value::Object(map) => map.values_mut().for_each(soften_all),
        _ => {}
    }
}

/// Installs the softer dark theme; the next switch to dark mode loads it.
pub fn install_softer_dark(cx: &mut App) {
    let dark = Theme::global(cx).dark_theme.clone();
    let Ok(mut value) = serde_json::to_value(&*dark) else { return };
    soften_all(&mut value);
    let parsed = serde_json::from_value::<ThemeConfig>(value);
    if let Ok(config) = parsed {
        Theme::update(cx, |theme| theme.dark_theme = Rc::new(config));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_colors_are_replaced_and_alpha_is_kept() {
        assert_eq!(soften("neutral-950").as_deref(), Some("#23262d"));
        assert_eq!(soften("#0a0a0a").as_deref(), Some("#23262d"));
        assert_eq!(soften("#17171766").as_deref(), Some("#1c1e2366"));
        assert_eq!(soften("neutral-800/60").as_deref(), Some("#34384299"));
        assert_eq!(soften("#1e40af33"), None);
        assert_eq!(soften("neutral-50"), None);
    }
}
