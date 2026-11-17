//! Wire format of the Quake's two HID interfaces, as used by DK-Suite 0.4.95.
//!
//! Control interface (0x4158:0x514B, usage page 0xFF60 / usage 0x61, 32-byte reports):
//!   host -> device  [0xA3, len, type, payload.., checksum]
//!                   len = payload.len() + 1, checksum = (type + sum(payload)) % 255,
//!                   type 1 = set, type 2 = get
//!   device -> host  [0xA3, len, op, cmd, data.., checksum]
//!                   checksum = sum(bytes[2 .. 2+len]) % 255
//!
//! Touch interface (0x0712:0x0010, usage page 0xFF73 / usage 0x71):
//!   [0xA3, len, op, 0x1A, count, {action, y_lo, y_hi, x_lo, x_hi} * count, ..]
//!   x/y are pixels on the 1920x480 screen; action 0 = released.
#![allow(dead_code)]

pub const HEADER: u8 = 0xA3;
pub const TYPE_SET: u8 = 1;
pub const TYPE_GET: u8 = 2;

// Commands. DK-Suite also has 0x04 (reboots the screen) and 0x2F 0x03 (enters
// firmware-update mode); `frame` refuses both. Unused ones are kept as documentation.
pub const CMD_BUZZER: u8 = 0x02; // set [level]; 0 = off
pub const CMD_MIC: u8 = 0x03; // get, or set [0|1]
pub const CMD_LUMINANCE: u8 = 0x05; // get, or set [0..=255]
pub const CMD_KNOB_LED: u8 = 0x06; // set [0 = on, 1 = off]
pub const CMD_VERSION: u8 = 0x2E; // get -> [name, major, minor, patch]
pub const CMD_PING: u8 = 0xEF; // get -> pong

pub const OP_INPUT: u8 = 0x03; // device events
pub const OP_REPLY: u8 = 0x55; // replies to our commands
pub const INPUT_KNOB_TURN: u8 = 0x01;
pub const INPUT_KNOB_PRESS: u8 = 0x02;
pub const REPLY_ACK: u8 = 0x00; // data[0] == 0x90 means success

const BLOCKED: [u8; 2] = [0x04, 0x2F];

/// Builds a control frame, without the leading HID report id.
pub fn frame(ty: u8, payload: &[u8]) -> Vec<u8> {
    assert!(!payload.is_empty() && !BLOCKED.contains(&payload[0]), "refusing command {payload:02x?}");
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.push(HEADER);
    out.push(payload.len() as u8 + 1);
    out.push(ty);
    out.extend_from_slice(payload);
    let sum: u32 = ty as u32 + payload.iter().map(|&b| b as u32).sum::<u32>();
    out.push((sum % 255) as u8);
    out
}

/// A control frame as a 33-byte HID output report (report id 0 + 32 bytes).
pub fn report(ty: u8, payload: &[u8]) -> [u8; 33] {
    let f = frame(ty, payload);
    let mut r = [0u8; 33];
    r[1..1 + f.len()].copy_from_slice(&f);
    r
}

// ---- VIA (QMK) protocol, same raw-HID interface, no 0xA3 framing ----
// [cmd, args..] in a 32-byte report; the device echoes the command with data.
pub const VIA_PROTOCOL_VERSION: u8 = 0x01;
pub const VIA_CUSTOM_SET: u8 = 0x07;
pub const VIA_CUSTOM_GET: u8 = 0x08;
/// The knob ring: VIA's built-in "qmk_rgb_matrix" menu.
pub const RGB_MATRIX_CHANNEL: u8 = 3;
pub const RGB_BRIGHTNESS: u8 = 1;
pub const RGB_EFFECT: u8 = 2;
pub const RGB_EFFECT_SPEED: u8 = 3;
pub const RGB_COLOR: u8 = 4; // [hue, saturation], 0..=255 each
pub const EFFECT_OFF: u8 = 0;
pub const EFFECT_SOLID: u8 = 1;
pub const EFFECT_BREATHING: u8 = 5;
pub const EFFECT_CYCLE_ALL: u8 = 12;

/// A VIA request as a 33-byte output report. Only reads and live (unsaved) ring
/// changes are allowed: no EEPROM saves, keymap writes or resets.
pub fn via_report(cmd: u8, args: &[u8]) -> [u8; 33] {
    let allowed = match cmd {
        VIA_PROTOCOL_VERSION | VIA_CUSTOM_GET => true,
        VIA_CUSTOM_SET => args.len() >= 2 && args[0] == RGB_MATRIX_CHANNEL && (RGB_BRIGHTNESS..=RGB_COLOR).contains(&args[1]),
        _ => false,
    };
    assert!(allowed, "refusing VIA command {cmd:#04x} {args:02x?}");
    let mut r = [0u8; 33];
    r[1] = cmd;
    r[2..2 + args.len()].copy_from_slice(args);
    r
}

/// Keyboard emulation: the Quake types a key combo as a real USB keyboard.
/// action: 1 = down, 0 = up, 2 = tap. modifiers: HID bits (1 ctrl, 2 shift, 4 alt, 8 cmd).
pub const CMD_KEYBOARD: u8 = 0x10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub op: u8,
    pub cmd: u8,
    pub data: Vec<u8>,
}

/// Parses a control-interface input report (no report id prefix).
pub fn parse(buf: &[u8]) -> Option<Message> {
    if buf.len() < 5 || buf[0] != HEADER {
        return None;
    }
    let len = buf[1] as usize;
    if len < 2 || buf.len() < 3 + len {
        return None;
    }
    let sum: u32 = buf[2..2 + len].iter().map(|&b| b as u32).sum();
    if (sum % 255) as u8 != buf[2 + len] {
        return None;
    }
    Some(Message { op: buf[2], cmd: buf[3], data: buf[4..2 + len].to_vec() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPoint {
    pub down: bool,
    pub x: u16,
    pub y: u16,
}

/// Parses a touch report. Returns None for reports that are not touch frames.
pub fn parse_touch(buf: &[u8]) -> Option<Vec<TouchPoint>> {
    if buf.len() < 5 || buf[0] != HEADER || buf[3] != 0x1A {
        return None;
    }
    let count = buf[4] as usize;
    let mut pts = Vec::with_capacity(count);
    for i in 0..count {
        let o = 5 + 5 * i;
        if o + 5 > buf.len() {
            break;
        }
        pts.push(TouchPoint {
            down: buf[o] != 0,
            y: u16::from(buf[o + 2]) << 8 | u16::from(buf[o + 1]),
            x: u16::from(buf[o + 4]) << 8 | u16::from(buf[o + 3]),
        });
    }
    Some(pts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_matches_dk_suite_capture() {
        // DK-Suite log: "HID sendShortCMD sent Data: [0x0,0xa3,0x2,0x2,0xef,0xf1]"
        assert_eq!(frame(TYPE_GET, &[CMD_PING]), vec![0xA3, 0x02, 0x02, 0xEF, 0xF1]);
        let r = report(TYPE_GET, &[CMD_PING]);
        assert_eq!(&r[..6], &[0x00, 0xA3, 0x02, 0x02, 0xEF, 0xF1]);
    }

    #[test]
    fn pong_matches_dk_suite_capture() {
        // DK-Suite log: "Received Sliced Data: [0xa3,0x3,0x55,0xef,0x38,0x7d,0x0,...]"
        let mut buf = vec![0xA3, 0x03, 0x55, 0xEF, 0x38, 0x7D];
        buf.resize(32, 0);
        assert_eq!(parse(&buf), Some(Message { op: OP_REPLY, cmd: CMD_PING, data: vec![0x38] }));
    }

    #[test]
    fn bad_checksum_is_rejected() {
        assert_eq!(parse(&[0xA3, 0x03, 0x55, 0xEF, 0x38, 0x7E]), None);
    }

    #[test]
    fn touch_point_layout() {
        // one point, down, x = 0x0394 (916), y = 0x015C (348)
        let buf = [0xA3, 0x08, 0x00, 0x1A, 0x01, 0x01, 0x5C, 0x01, 0x94, 0x03];
        assert_eq!(parse_touch(&buf), Some(vec![TouchPoint { down: true, x: 916, y: 348 }]));
    }

    #[test]
    fn via_ring_reports() {
        let r = via_report(VIA_CUSTOM_SET, &[RGB_MATRIX_CHANNEL, RGB_COLOR, 20, 255]);
        assert_eq!(&r[..6], &[0x00, 0x07, 0x03, 0x04, 20, 255]);
    }

    #[test]
    #[should_panic]
    fn via_save_is_refused() {
        via_report(0x09, &[RGB_MATRIX_CHANNEL]);
    }

    #[test]
    #[should_panic]
    fn screen_reboot_is_refused() {
        frame(TYPE_SET, &[0x04, 0x00]);
    }

    #[test]
    #[should_panic]
    fn firmware_mode_is_refused() {
        frame(TYPE_SET, &[0x2F, 0x03]);
    }
}
