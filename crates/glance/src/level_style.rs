//! How a Level looks, shared by the Entry view and the Filter bar so the two agree.

use glance_core::level::Level;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// Colors taken from the theme once; copy it into closures.
#[derive(Clone, Copy)]
pub struct LevelPalette {
    warn: Hsla,
    danger: Hsla,
    muted: Hsla,
}

pub struct LevelStyle {
    /// Background of the Entry (or of the Level's label).
    pub tint: Option<Hsla>,
    /// TRACE and DEBUG are drawn in the muted text color.
    pub faded: bool,
}

impl LevelPalette {
    pub fn new(cx: &App) -> Self {
        let theme = cx.theme();
        Self { warn: theme.warning, danger: theme.danger, muted: theme.muted_foreground }
    }

    pub fn muted(&self) -> Hsla {
        self.muted
    }

    pub fn style(&self, level: Option<Level>) -> LevelStyle {
        let tint = match level {
            Some(Level::Warn) => Some(self.warn.opacity(0.16)),
            Some(Level::Error) => Some(self.danger.opacity(0.16)),
            Some(Level::Fatal) => Some(self.danger.opacity(0.32)),
            _ => None,
        };
        LevelStyle { tint, faded: matches!(level, Some(Level::Trace | Level::Debug)) }
    }
}
