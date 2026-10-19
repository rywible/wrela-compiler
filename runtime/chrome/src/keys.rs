//! A key press as DevTools' `Input.dispatchKeyEvent` takes it, from the key's number in
//! runtime/abi `input` ([`wrela_abi::input::KEYS`], `std::input::Key`'s numbers) and the
//! modifiers held: its DOM `key` and `code`, its Windows virtual key code, and the text it types
//! on a US layout (none with Control or Meta, which make it a shortcut).

use serde_json::{Value, json};

/// The modifiers held, as `std::input::Mods` has them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Mods {
    /// DevTools' bit field: Alt 1, Control 2, Meta 4, Shift 8.
    fn bits(self) -> u32 {
        u32::from(self.alt)
            | u32::from(self.ctrl) << 1
            | u32::from(self.meta) << 2
            | u32::from(self.shift) << 3
    }
}

/// The DOM `key`, the virtual key code and the text of the key `code` (a DOM code), with Shift
/// held or not: `None` for a key a script can't press.
fn describe(code: &str, shift: bool) -> Option<(String, u32, Option<String>)> {
    let typed = |plain: &str, shifted: &str, vk: u32| {
        let k = if shift { shifted } else { plain };
        Some((k.to_string(), vk, Some(k.to_string())))
    };
    let named = |key: &str, vk: u32| Some((key.to_string(), vk, None));
    if let Some(letter) = code.strip_prefix("Key")
        && letter.len() == 1
    {
        let c = letter.chars().next()?;
        let vk = u32::from(c);
        return typed(&c.to_ascii_lowercase().to_string(), &c.to_string(), vk);
    }
    if let Some(digit) = code.strip_prefix("Digit")
        && digit.len() == 1
    {
        let vk = u32::from(digit.chars().next()?);
        let shifted = ")!@#$%^&*(".chars().nth(digit.parse::<usize>().ok()?)?.to_string();
        return typed(digit, &shifted, vk);
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=12).contains(&n)
    {
        return named(code, 111 + n);
    }
    match code {
        "Enter" | "NumpadEnter" => Some(("Enter".into(), 13, Some("\r".into()))),
        "Space" => typed(" ", " ", 32),
        "Escape" => named("Escape", 27),
        "Backspace" => named("Backspace", 8),
        "Tab" => named("Tab", 9),
        "ArrowLeft" => named("ArrowLeft", 37),
        "ArrowUp" => named("ArrowUp", 38),
        "ArrowRight" => named("ArrowRight", 39),
        "ArrowDown" => named("ArrowDown", 40),
        "Home" => named("Home", 36),
        "End" => named("End", 35),
        "PageUp" => named("PageUp", 33),
        "PageDown" => named("PageDown", 34),
        "Insert" => named("Insert", 45),
        "Delete" => named("Delete", 46),
        "ShiftLeft" | "ShiftRight" => named("Shift", 16),
        "ControlLeft" | "ControlRight" => named("Control", 17),
        "AltLeft" | "AltRight" => named("Alt", 18),
        "MetaLeft" | "MetaRight" => named("Meta", 91),
        "CapsLock" => named("CapsLock", 20),
        "Minus" => typed("-", "_", 189),
        "Equal" => typed("=", "+", 187),
        "BracketLeft" => typed("[", "{", 219),
        "BracketRight" => typed("]", "}", 221),
        "Backslash" => typed("\\", "|", 220),
        "Semicolon" => typed(";", ":", 186),
        "Quote" => typed("'", "\"", 222),
        "Backquote" => typed("`", "~", 192),
        "Comma" => typed(",", "<", 188),
        "Period" => typed(".", ">", 190),
        "Slash" => typed("/", "?", 191),
        "NumpadAdd" => typed("+", "+", 107),
        "NumpadSubtract" => typed("-", "-", 109),
        "NumpadMultiply" => typed("*", "*", 106),
        "NumpadDivide" => typed("/", "/", 111),
        "NumpadDecimal" => typed(".", ".", 110),
        _ => None,
    }
}

/// The `keyDown` and `keyUp` parameters of a press of key number `key` with `mods` held.
pub(crate) fn press(key: u32, mods: Mods) -> Result<(Value, Value), String> {
    let code = wrela_abi::input::KEYS
        .get(key as usize)
        .filter(|_| key != 0)
        .ok_or_else(|| format!("{key} isn't a key's number"))?;
    let (k, vk, text) =
        describe(code, mods.shift).ok_or_else(|| format!("a script can't press {code}"))?;
    let mut down = json!({"type": "keyDown", "key": k, "code": code, "windowsVirtualKeyCode": vk,
                          "modifiers": mods.bits()});
    if let Some(t) = text.filter(|_| !mods.ctrl && !mods.meta) {
        down["text"] = Value::String(t);
    }
    let up = json!({"type": "keyUp", "key": k, "code": code, "windowsVirtualKeyCode": vk,
                    "modifiers": mods.bits()});
    Ok((down, up))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wrela_abi::input::key_code;

    #[test]
    fn a_press_is_the_key_its_code_and_text_says() {
        let (down, up) = press(key_code("KeyA"), Mods::default()).expect("a");
        assert_eq!(down["key"], "a");
        assert_eq!(down["text"], "a");
        assert_eq!(down["windowsVirtualKeyCode"], 65);
        assert_eq!(up["type"], "keyUp");
        assert!(up.get("text").is_none());
        let (down, _) =
            press(key_code("KeyB"), Mods { shift: true, ..Mods::default() }).expect("B");
        assert_eq!(
            (down["key"].as_str(), down["text"].as_str(), down["modifiers"].as_u64()),
            (Some("B"), Some("B"), Some(8))
        );
        let (enter, _) = press(key_code("Enter"), Mods::default()).expect("enter");
        assert_eq!(
            (enter["text"].as_str(), enter["windowsVirtualKeyCode"].as_u64()),
            (Some("\r"), Some(13))
        );
        let (back, _) = press(key_code("Backspace"), Mods::default()).expect("backspace");
        assert!(back.get("text").is_none());
        let (space, _) = press(key_code("Space"), Mods::default()).expect("space");
        assert_eq!(space["text"], " ");
        let (copy, _) =
            press(key_code("KeyC"), Mods { meta: true, ..Mods::default() }).expect("copy");
        assert!(copy.get("text").is_none(), "a shortcut types nothing");
        assert!(press(0, Mods::default()).is_err());
        assert!(press(10_000, Mods::default()).is_err());
    }
}
