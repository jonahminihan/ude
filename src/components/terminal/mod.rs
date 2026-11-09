mod colors;
mod element;
mod keys;
mod mouse;

use std::borrow::Cow;
use std::cell::{Cell as StdCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode, viewport_to_point};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{
    CursorShape, CursorStyle, NamedColor, Processor, StdSyncHandler,
};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::*;

use colors::Palette;
use element::{CellMetrics, CursorState, GridCache};
use mouse::MouseAction;

#[cfg(target_os = "windows")]
const FONT_FAMILY: &str = "Cascadia Mono";
#[cfg(target_os = "macos")]
const FONT_FAMILY: &str = "Menlo";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const FONT_FAMILY: &str = "DejaVu Sans Mono";

const FONT_SIZE: Pixels = px(14.);
const LINE_HEIGHT: f32 = 1.3;
const PADDING: Pixels = px(8.);
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Programs scroll several lines per wheel report, so a report is sent per notch rather than per
/// line (Windows reports one notch as three lines by default).
const LINES_PER_WHEEL_REPORT: f32 = 3.;
const DEFAULT_TITLE: &str = "Terminal";
const CONTEXT: &str = "Terminal";

actions!(terminal, [Copy, Paste]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Root claims these for focus navigation and copying; the shell needs them instead.
        KeyBinding::new("tab", NoAction {}, Some(CONTEXT)),
        KeyBinding::new("shift-tab", NoAction {}, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", NoAction {}, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", Paste, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-c", Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-v", Paste, Some(CONTEXT)),
        KeyBinding::new("shift-insert", Paste, Some(CONTEXT)),
    ]);
}

pub enum TerminalEvent {
    TitleChanged,
}

#[derive(Clone)]
struct EventProxy(UnboundedSender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

/// Where the grid was last painted, so mouse positions can be mapped back to cells.
#[derive(Clone, Copy)]
struct GridLayout {
    origin: Point<Pixels>,
    columns: usize,
    lines: usize,
}

pub struct Terminal {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    focus_handle: FocusHandle,
    palette: Rc<Palette>,
    metrics: CellMetrics,
    layout: Rc<StdCell<GridLayout>>,
    grid_cache: Rc<RefCell<GridCache>>,
    title: SharedString,
    exited: bool,
    scroll_remainder: Pixels,
    selecting: bool,
    last_reported_cell: Option<(usize, usize)>,
    marked_text: String,
    cursor_visible: bool,
    last_input: Instant,
    _tasks: [Task<()>; 2],
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Terminal {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let metrics = CellMetrics::measure(FONT_FAMILY, FONT_SIZE, LINE_HEIGHT, window);
        let layout = GridLayout {
            origin: Point::default(),
            columns: 80,
            lines: 24,
        };

        let (events_tx, mut events_rx) = unbounded();
        let proxy = EventProxy(events_tx);
        let config = Config {
            default_cursor_style: CursorStyle {
                shape: CursorShape::Block,
                blinking: true,
            },
            ..Default::default()
        };
        let term = Term::new(config, &TermSize::new(layout.columns, layout.lines), proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let options = tty::Options {
            working_directory: std::env::current_dir().ok(),
            env: HashMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ]),
            ..Default::default()
        };
        let pty = tty::new(
            &options,
            window_size(layout.columns, layout.lines, &metrics),
            0,
        )
        .expect("Failed to spawn shell");
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .expect("Failed to create terminal event loop");
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = events_rx.next().await {
                let mut batch = vec![event];
                while let Ok(event) = events_rx.try_recv() {
                    batch.push(event);
                }
                let updated = this.update(cx, |this, cx| {
                    for event in batch {
                        this.handle_event(event, cx);
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        let blink = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                if this.update_in(cx, |this, window, cx| this.blink(window, cx)).is_err() {
                    break;
                }
            }
        });

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let subscriptions = [
            cx.on_focus(&focus_handle, window, |this, _, cx| this.focus_changed(true, cx)),
            cx.on_blur(&focus_handle, window, |this, _, cx| this.focus_changed(false, cx)),
        ];

        Self {
            term,
            notifier,
            focus_handle,
            palette: Rc::new(Palette::default()),
            metrics,
            layout: Rc::new(StdCell::new(layout)),
            grid_cache: Rc::default(),
            title: DEFAULT_TITLE.into(),
            exited: false,
            scroll_remainder: px(0.),
            selecting: false,
            last_reported_cell: None,
            marked_text: String::new(),
            cursor_visible: true,
            last_input: Instant::now(),
            _tasks: [events, blink],
            _subscriptions: subscriptions,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => cx.notify(),
            Event::Title(title) => self.set_title(title.into(), cx),
            Event::ResetTitle => self.set_title(DEFAULT_TITLE.into(), cx),
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            Event::ClipboardStore(_, text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Event::ClipboardLoad(_, format) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.write(format(&text).into_bytes());
                }
            }
            Event::ColorRequest(index, format) => {
                let rgb = self.palette.rgb(index, self.term.lock().colors());
                self.write(format(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let layout = self.layout.get();
                let size = window_size(layout.columns, layout.lines, &self.metrics);
                self.write(format(size).into_bytes());
            }
            Event::ChildExit(status) => {
                self.exited = true;
                let message = match status.code() {
                    Some(code) => format!("\r\n[Process exited with code {code}]\r\n"),
                    None => "\r\n[Process exited]\r\n".to_owned(),
                };
                let mut term = self.term.lock();
                Processor::<StdSyncHandler>::new().advance(&mut *term, message.as_bytes());
                cx.notify();
            }
            Event::Exit => cx.notify(),
            Event::Bell => {}
        }
    }

    fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title;
            cx.emit(TerminalEvent::TitleChanged);
        }
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if !self.exited {
            self.notifier.notify(bytes);
        }
    }

    /// Writes user input, which also brings the viewport back to the prompt.
    fn input(&mut self, bytes: impl Into<Cow<'static, [u8]>>) {
        {
            let mut term = self.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        self.cursor_visible = true;
        self.last_input = Instant::now();
        self.write(bytes);
    }

    fn blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blinking = self.term.lock().cursor_style().blinking;        let typing = self.last_input.elapsed() < BLINK_INTERVAL;
        let visible = if blinking && !typing && self.focus_handle.is_focused(window) {
            !self.cursor_visible
        } else {
            true
        };
        if visible != self.cursor_visible {
            self.cursor_visible = visible;
            cx.notify();
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(if focused { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
        }
        self.cursor_visible = true;
        cx.notify();
    }

    fn viewport_cell(&self, position: Point<Pixels>) -> (usize, usize, Side) {
        let layout = self.layout.get();
        let x = ((position.x - layout.origin.x) / self.metrics.cell_width).max(0.);
        let y = ((position.y - layout.origin.y) / self.metrics.cell_height).max(0.);
        let column = (x as usize).min(layout.columns - 1);
        let line = (y as usize).min(layout.lines - 1);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (line, column, side)
    }

    fn grid_point(&self, position: Point<Pixels>, display_offset: usize) -> (GridPoint, Side) {
        let (line, column, side) = self.viewport_cell(position);
        let point = viewport_to_point(display_offset, GridPoint::new(line, Column(column)));
        (point, side)
    }

    /// Programs that enable mouse tracking get the mouse, unless shift is held to select text.
    fn mouse_mode(&self, modifiers: Modifiers) -> Option<TermMode> {
        let mode = *self.term.lock().mode();
        (mode.intersects(TermMode::MOUSE_MODE) && !modifiers.shift).then_some(mode)
    }

    fn report_mouse(&mut self, action: MouseAction, position: Point<Pixels>, modifiers: Modifiers, mode: TermMode) {
        let (line, column, _) = self.viewport_cell(position);
        if matches!(action, MouseAction::Drag(_) | MouseAction::Move)
            && self.last_reported_cell == Some((line, column))
        {
            return;
        }
        self.last_reported_cell = Some((line, column));
        if let Some(bytes) = mouse::report(action, line, column, modifiers, mode) {
            self.write(bytes);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Dead keys and AltGr compose through the platform, which delivers the result to the input handler.
        if event.prefer_character_input {
            return;
        }
        let mode = *self.term.lock().mode();
        let Some(text) = keys::to_esc_str(&event.keystroke, mode) else {
            return;
        };
        self.input(match text {
            Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
            Cow::Owned(text) => Cow::Owned(text.into_bytes()),
        });
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let text = if bracketed {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text
        };
        self.input(text.into_bytes());
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        if let Some(mode) = self.mouse_mode(event.modifiers) {
            self.report_mouse(MouseAction::Press(event.button), event.position, event.modifiers, mode);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }

        let term = self.term.clone();
        let mut term = term.lock();
        let (point, side) = self.grid_point(event.position, term.grid().display_offset());
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        term.selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(mode) = self.mouse_mode(event.modifiers) {
            let action = match event.pressed_button {
                Some(button) => MouseAction::Drag(button),
                None => MouseAction::Move,
            };
            self.report_mouse(action, event.position, event.modifiers, mode);
            return;
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }

        let term = self.term.clone();
        let mut term = term.lock();
        let (point, side) = self.grid_point(event.position, term.grid().display_offset());
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(mode) = self.mouse_mode(event.modifiers) {
            self.report_mouse(MouseAction::Release(event.button), event.position, event.modifiers, mode);
            return;
        }

        self.selecting = false;
        let mut term = self.term.lock();
        if term.selection.as_ref().is_some_and(Selection::is_empty) {
            term.selection = None;
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let cell_height = self.metrics.cell_height;
        self.scroll_remainder += event.delta.pixel_delta(cell_height).y;

        if let Some(mode) = self.mouse_mode(event.modifiers) {
            let step = cell_height * LINES_PER_WHEEL_REPORT;
            let reports = (self.scroll_remainder / step) as i32;
            self.scroll_remainder -= step * reports as f32;
            let action = if reports > 0 { MouseAction::WheelUp } else { MouseAction::WheelDown };
            for _ in 0..reports.unsigned_abs() {
                self.report_mouse(action, event.position, event.modifiers, mode);
            }
            return;
        }

        let lines = (self.scroll_remainder / cell_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= cell_height * lines as f32;

        let mut term = self.term.lock();
        let mode = *term.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen programs like less and vim get arrow keys instead of scrollback.
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            drop(term);
            self.write(arrow.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else {
            term.scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Receives text the platform composes outside of key-down events: IME commits, dead keys, AltGr
/// characters and injected Unicode input. The terminal has no editable buffer, so only the IME's
/// in-progress composition is tracked, to draw it at the cursor until it's committed.
impl EntityInputHandler for Terminal {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked_text.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text.clear();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text.clear();
        self.input(text.to_owned().into_bytes());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = new_text.to_owned();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.get();
        let term = self.term.lock();
        let cursor = term.grid().cursor.point;
        let row = cursor.line.0 + term.grid().display_offset() as i32;
        Some(Bounds::new(
            point(
                layout.origin.x + self.metrics.cell_width * cursor.column.0 as f32,
                layout.origin.y + self.metrics.cell_height * row as f32,
            ),
            size(self.metrics.cell_width, self.metrics.cell_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for Terminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let term = self.term.clone();
        let palette = self.palette.clone();
        let metrics = self.metrics.clone();
        let layout = self.layout.clone();
        let grid_cache = self.grid_cache.clone();
        let sender: EventLoopSender = self.notifier.0.clone();
        let focus_handle = self.focus_handle.clone();
        let cursor = CursorState {
            focused: focus_handle.is_focused(window),
            visible: self.cursor_visible,
            marked_text: self.marked_text.clone(),
        };
        let entity = cx.entity();
        let background = palette.named(NamedColor::Background, term.lock().colors());

        div()
            .track_focus(&self.focus_handle)
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll))
            .size_full()
            .p(PADDING)
            .bg(background)
            .cursor_text()
            .child(
                canvas(
                    move |bounds, _, _| {
                        let columns = ((bounds.size.width / metrics.cell_width) as usize).max(2);
                        let lines = ((bounds.size.height / metrics.cell_height) as usize).max(1);
                        let previous = layout.replace(GridLayout {
                            origin: bounds.origin,
                            columns,
                            lines,
                        });
                        if (previous.columns, previous.lines) != (columns, lines) {
                            term.lock().resize(TermSize::new(columns, lines));
                            let _ = sender.send(Msg::Resize(window_size(columns, lines, &metrics)));
                        }
                        (term, palette, metrics)
                    },
                    move |bounds, (term, palette, metrics), window, cx| {
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(bounds, entity),
                            cx,
                        );
                        let mut term = term.lock();
                        element::paint_grid(
                            &mut term,
                            &mut grid_cache.borrow_mut(),
                            bounds.origin,
                            &metrics,
                            &palette,
                            &cursor,
                            window,
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}
fn window_size(columns: usize, lines: usize, metrics: &CellMetrics) -> WindowSize {
    WindowSize {
        num_lines: lines as u16,
        num_cols: columns as u16,
        cell_width: f32::from(metrics.cell_width) as u16,
        cell_height: f32::from(metrics.cell_height) as u16,
    }
}
