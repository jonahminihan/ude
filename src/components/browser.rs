use gpui_kit::*;
use gpui_webview::WebView;

pub struct Browser {
    webview: Entity<WebView>,
}

impl Browser {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let builder = wry::WebViewBuilder::new()
            .with_url("https://google.com")
            .with_focused(false);
        let webview = WebView::build(builder, window, cx).expect("Failed to create webview");

        Self {
            webview: cx.new(|_| webview),
        }
    }
}

impl Render for Browser {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.webview.clone())
    }
}
