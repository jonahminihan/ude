use alacritty_terminal::term::TermMode;
use gpui_kit::{Modifiers, MouseButton};

/// What happened to the mouse, in the terms xterm's mouse protocol reports.
#[derive(Clone, Copy)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    Drag(MouseButton),
    Move,
    WheelUp,
    WheelDown,
}

/// Encodes a mouse report for a cell in the viewport, or `None` when the program didn't ask for it.
pub fn report(
    action: MouseAction,
    line: usize,
    column: usize,
    modifiers: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let wanted = match action {
        MouseAction::Press(_) | MouseAction::Release(_) | MouseAction::WheelUp | MouseAction::WheelDown => {
            mode.intersects(TermMode::MOUSE_MODE)
        }
        MouseAction::Drag(_) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
        MouseAction::Move => mode.contains(TermMode::MOUSE_MOTION),
    };
    if !wanted {
        return None;
    }

    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code = match action {
        MouseAction::Press(button) | MouseAction::Drag(button) => button_code(button)?,
        // Only SGR can say which button was released; the legacy encoding uses 3 for all of them.
        MouseAction::Release(button) if sgr => button_code(button)?,
        MouseAction::Release(_) => 3,
        MouseAction::Move => 3,
        MouseAction::WheelUp => 64,
        MouseAction::WheelDown => 65,
    };
    if matches!(action, MouseAction::Drag(_) | MouseAction::Move) {
        code += 32;
    }
    code += (modifiers.shift as u8) * 4 + (modifiers.alt as u8) * 8 + (modifiers.control as u8) * 16;

    if sgr {
        let suffix = if matches!(action, MouseAction::Release(_)) { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", column + 1, line + 1).into_bytes());
    }

    let mut bytes = b"\x1b[M".to_vec();
    bytes.push(32 + code);
    if mode.contains(TermMode::UTF8_MOUSE) {
        for position in [column, line] {
            let value = char::from_u32(32 + 1 + position as u32)?;
            let mut buffer = [0; 4];
            bytes.extend_from_slice(value.encode_utf8(&mut buffer).as_bytes());
        }
    } else {
        // The legacy encoding stores each coordinate in a single byte.
        for position in [column, line] {
            bytes.push(u8::try_from(32 + 1 + position).ok()?);
        }
    }
    Some(bytes)
}

fn button_code(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_press_and_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let press = report(MouseAction::Press(MouseButton::Left), 4, 9, Modifiers::default(), mode);
        assert_eq!(press.as_deref(), Some(&b"\x1b[<0;10;5M"[..]));
        let release = report(MouseAction::Release(MouseButton::Right), 0, 0, Modifiers::default(), mode);
        assert_eq!(release.as_deref(), Some(&b"\x1b[<2;1;1m"[..]));
    }

    #[test]
    fn legacy_encoding_offsets_by_32() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let press = report(MouseAction::Press(MouseButton::Left), 0, 0, Modifiers::default(), mode);
        assert_eq!(press.as_deref(), Some(&b"\x1b[M !!"[..]));
    }

    #[test]
    fn motion_needs_motion_mode() {
        let drag_only = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert!(report(MouseAction::Move, 0, 0, Modifiers::default(), drag_only).is_none());
        let drag = report(MouseAction::Drag(MouseButton::Left), 1, 1, Modifiers::default(), drag_only);
        assert_eq!(drag.as_deref(), Some(&b"\x1b[<32;2;2M"[..]));
    }
}
