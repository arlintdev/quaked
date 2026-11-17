//! Key combos like "cmd+shift+4" -> (HID modifier bits, HID usage code), for the
//! Quake's keyboard emulation (it types them as a real USB keyboard).

pub const CTRL: u8 = 0x01;
pub const SHIFT: u8 = 0x02;
pub const ALT: u8 = 0x04;
pub const CMD: u8 = 0x08;

fn key_code(name: &str) -> Option<u8> {
    let n = name.to_lowercase();
    let b = n.as_bytes();
    if b.len() == 1 {
        let c = b[0];
        return match c {
            b'a'..=b'z' => Some(4 + (c - b'a')),
            b'1'..=b'9' => Some(30 + (c - b'1')),
            b'0' => Some(39),
            b'-' => Some(45),
            b'=' => Some(46),
            b'[' => Some(47),
            b']' => Some(48),
            b'\\' => Some(49),
            b';' => Some(51),
            b'\'' => Some(52),
            b'`' => Some(53),
            b',' => Some(54),
            b'.' => Some(55),
            b'/' => Some(56),
            _ => None,
        };
    }
    if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
        return match f {
            1..=12 => Some(58 + f - 1),
            13..=24 => Some(104 + f - 13),
            _ => None,
        };
    }
    Some(match n.as_str() {
        "return" | "enter" => 40,
        "escape" | "esc" => 41,
        "delete" | "backspace" => 42,
        "tab" => 43,
        "space" => 44,
        "minus" => 45,
        "equal" => 46,
        "forwarddelete" | "del" => 76,
        "home" => 74,
        "pageup" => 75,
        "end" => 77,
        "pagedown" => 78,
        "right" => 79,
        "left" => 80,
        "down" => 81,
        "up" => 82,
        "mute" => 127,
        "volumeup" => 128,
        "volumedown" => 129,
        _ => return None,
    })
}

/// Parses "cmd+shift+4", "ctrl+up", "f5". Returns None for anything unknown.
pub fn parse(combo: &str) -> Option<(u8, u8)> {
    let mut mods = 0u8;
    let mut key = None;
    for part in combo.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "cmd" | "command" | "super" => mods |= CMD,
            "ctrl" | "control" => mods |= CTRL,
            "alt" | "opt" | "option" => mods |= ALT,
            "shift" => mods |= SHIFT,
            other => key = Some(key_code(other)?),
        }
    }
    Some((mods, key?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        assert_eq!(parse("cmd+shift+4"), Some((CMD | SHIFT, 33)));
        assert_eq!(parse("ctrl+cmd+q"), Some((CTRL | CMD, 20)));
        assert_eq!(parse("ctrl+up"), Some((CTRL, 82)));
        assert_eq!(parse("F5"), Some((0, 62)));
        assert_eq!(parse("cmd+shift"), None);
        assert_eq!(parse("cmd+wat"), None);
    }
}
