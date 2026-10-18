mod app;
mod filter_bar;
mod highlight_bar;
mod level_style;
mod search_bar;
mod theme;
mod tab_view;

use std::path::PathBuf;

use gpui_kit::*;

use app::GlanceApp;
use tab_view::{CloseSearch, CopySelection, OpenSearch};

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    // The bundled icons: components such as the Checkbox draw their marks from them.
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        theme::install_softer_dark(cx);
        let copy = if cfg!(target_os = "macos") { "cmd-c" } else { "ctrl-c" };
        let find = if cfg!(target_os = "macos") { "cmd-f" } else { "ctrl-f" };
        cx.bind_keys([
            KeyBinding::new(copy, CopySelection, Some("EntryView")),
            KeyBinding::new(find, OpenSearch, None),
            KeyBinding::new("escape", CloseSearch, Some("SearchBar")),
        ]);
        let state = GlanceApp::load_state();
        let size = state.window.map_or(size(px(1100.), px(720.)), |w| size(px(w.width as f32), px(w.height as f32)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            ..WindowOptions::default()
        };
        gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| GlanceApp::new(args.clone(), state.clone(), window, cx))
        })
            .expect("failed to open window");
        cx.activate(true);
    });
}
