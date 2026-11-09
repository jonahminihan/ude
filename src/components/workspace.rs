use gpui_kit::*;

use super::browser::Browser;
use super::terminal::{Terminal, TerminalEvent};

pub struct Workspace {
    browser: Entity<Browser>,
    terminal: Entity<Terminal>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let browser = Browser::new(window, cx);
        let terminal = cx.new(|cx| Terminal::new(window, cx));
        let subscription = cx.subscribe_in(&terminal, window, |_, terminal, event, window, cx| {
            match event {
                TerminalEvent::TitleChanged => {
                    window.set_window_title(&format!("ude - {}", terminal.read(cx).title()));
                }
            }
        });

        Self {
            browser: cx.new(|_| browser),
            terminal,
            _subscription: subscription,
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(self.browser.clone()))
            .child(div().h(px(320.)).flex_none().child(self.terminal.clone()))
    }
}
