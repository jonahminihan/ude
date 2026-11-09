mod components;

use components::workspace::Workspace;
use gpui_kit::*;

fn main() {
    // GPUI's DirectComposition renderer draws over the WebView2 surface on Windows.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        components::terminal::init(cx);

        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
