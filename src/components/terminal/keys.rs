use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use gpui_kit::{Keystroke, Modifiers};

/// Encodes a keystroke as the bytes an xterm-compatible terminal would send to the PTY.
pub fn to_esc_str(keystroke: &Keystroke, mode: TermMode) -> Option<Cow<'static, str>> {
    let modifiers = keystroke.modifiers;
    let param = modifier_param(modifiers);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    let named = match keystroke.key.as_str() {
        "enter" if modifiers.alt => Some("\x1b\r"),
        "enter" => Some("\r"),
        "tab" if modifiers.shift => Some("\x1b[Z"),
        "tab" => Some("\t"),
        "escape" => Some("\x1b"),
        "backspace" if modifiers.control => Some("\x08"),
        "backspace" if modifiers.alt => Some("\x1b\x7f"),
        "backspace" => Some("\x7f"),
        "space" if modifiers.control => Some("\x00"),
        _ => None,
    };
    if let Some(named) = named {
        return Some(Cow::Borrowed(named));
    }

    if let Some(final_byte) = cursor_key(&keystroke.key) {
        return Some(match param {
            Some(param) => Cow::Owned(format!("\x1b[1;{param}{final_byte}")),
            None if app_cursor => Cow::Owned(format!("\x1bO{final_byte}")),
            None => Cow::Owned(format!("\x1b[{final_byte}")),
        });
    }

    if let Some(code) = tilde_key(&keystroke.key) {
        return Some(match param {
            Some(param) => Cow::Owned(format!("\x1b[{code};{param}~")),
            None => Cow::Owned(format!("\x1b[{code}~")),
        });
    }

    if let Some(final_byte) = ss3_function_key(&keystroke.key) {
        return Some(match param {
            Some(param) => Cow::Owned(format!("\x1b[1;{param}{final_byte}")),
            None => Cow::Owned(format!("\x1bO{final_byte}")),
        });
    }

    // AltGr arrives as ctrl+alt, so a typed character wins over the ctrl and alt encodings.
    let altgr = modifiers.control && modifiers.alt && keystroke.key_char.is_some();
    if modifiers.control && !altgr {
        if let Some(byte) = control_byte(&keystroke.key) {
            let prefix = if modifiers.alt { "\x1b" } else { "" };
            return Some(Cow::Owned(format!("{prefix}{}", byte as char)));
        }
    }

    let text = keystroke.key_char.as_deref()?;
    if modifiers.alt && !altgr {
        Some(Cow::Owned(format!("\x1b{text}")))
    } else {
        Some(Cow::Owned(text.to_owned()))
    }
}

fn modifier_param(modifiers: Modifiers) -> Option<u8> {
    let param = 1
        + modifiers.shift as u8
        + ((modifiers.alt as u8) << 1)
        + ((modifiers.control as u8) << 2);
    (param > 1).then_some(param)
}

fn cursor_key(key: &str) -> Option<char> {
    Some(match key {
        "up" => 'A',
        "down" => 'B',
        "right" => 'C',
        "left" => 'D',
        "home" => 'H',
        "end" => 'F',
        _ => return None,
    })
}

fn ss3_function_key(key: &str) -> Option<char> {
    Some(match key {
        "f1" => 'P',
        "f2" => 'Q',
        "f3" => 'R',
        "f4" => 'S',
        _ => return None,
    })
}

fn tilde_key(key: &str) -> Option<u8> {
    Some(match key {
        "insert" => 2,
        "delete" => 3,
        "pageup" => 5,
        "pagedown" => 6,
        "f5" => 15,
        "f6" => 17,
        "f7" => 18,
        "f8" => 19,
        "f9" => 20,
        "f10" => 21,
        "f11" => 23,
        "f12" => 24,
        _ => return None,
    })
}

fn control_byte(key: &str) -> Option<u8> {
    let &[byte] = key.as_bytes() else {
        return None;
    };
    Some(match byte {
        b'a'..=b'z' => byte - b'a' + 1,
        b'@' | b'2' => 0x00,
        b'[' | b'3' => 0x1b,
        b'\\' | b'4' => 0x1c,
        b']' | b'5' => 0x1d,
        b'^' | b'6' => 0x1e,
        b'_' | b'-' | b'7' => 0x1f,
        b'/' => 0x1f,
        b'8' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &'static str) -> Keystroke {
        Keystroke::parse(source).unwrap()
    }

    fn encode(source: &'static str, mode: TermMode) -> Option<String> {
        to_esc_str(&key(source), mode).map(Cow::into_owned)
    }

    #[test]
    fn control_letters_map_to_c0_codes() {
        assert_eq!(encode("ctrl-c", TermMode::NONE).as_deref(), Some("\x03"));
        assert_eq!(encode("ctrl-alt-c", TermMode::NONE).as_deref(), Some("\x1b\x03"));
        assert_eq!(encode("ctrl-[", TermMode::NONE).as_deref(), Some("\x1b"));
    }

    #[test]
    fn arrows_respect_app_cursor_mode() {
        assert_eq!(encode("up", TermMode::NONE).as_deref(), Some("\x1b[A"));
        assert_eq!(encode("up", TermMode::APP_CURSOR).as_deref(), Some("\x1bOA"));
        assert_eq!(encode("ctrl-left", TermMode::NONE).as_deref(), Some("\x1b[1;5D"));
    }

    #[test]
    fn special_keys() {
        assert_eq!(encode("enter", TermMode::NONE).as_deref(), Some("\r"));
        assert_eq!(encode("shift-tab", TermMode::NONE).as_deref(), Some("\x1b[Z"));
        assert_eq!(encode("delete", TermMode::NONE).as_deref(), Some("\x1b[3~"));
        assert_eq!(encode("f5", TermMode::NONE).as_deref(), Some("\x1b[15~"));
    }
}
