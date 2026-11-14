//! The tx-ide palette (Tokyo Night), defined once. The RGB constants are the source; the tmux
//! hex strings and the ANSI foreground escapes are derived from them at compile time.
//! `tx-client/tmux/keys.tmux` is shell and repeats two of the hex values by hand.
//!
//! Every ratatui surface routes its colours through [`palette`], so `TX_HOME_COLORS=256` swaps
//! them for the nearest entries of the 256-colour palette.

use std::sync::OnceLock;

use ratatui_core::style::Color;

/// A `&'static str` of the bytes `$bytes` (a const expression) produces, cut at `$length`.
macro_rules! static_text {
    ($bytes:expr, $length:expr) => {{
        const BYTES: &[u8] = $bytes.split_at($length).0;
        match std::str::from_utf8(BYTES) {
            Ok(text) => text,
            Err(_) => panic!("palette text is ASCII"),
        }
    }};
}

macro_rules! hex {
    ($color:expr) => {
        static_text!(&hex_bytes($color), 7)
    };
}

macro_rules! foreground_ansi {
    ($color:expr) => {
        static_text!(&ansi_bytes($color).0, ansi_bytes($color).1)
    };
}

/// "This is tx" / "this is selected": the active pane border, headers, the selected row.
pub const ACCENT: Color = Color::Rgb(122, 162, 247);
/// Text on an accent background.
pub const ON_ACCENT: Color = Color::Rgb(26, 27, 38);
pub const INFO: Color = Color::Rgb(125, 207, 255);
/// In transition, look at this.
pub const WARN: Color = Color::Rgb(224, 175, 104);
/// Primary text.
pub const FG: Color = Color::Rgb(192, 202, 245);
/// Secondary text, always with the DIM modifier.
pub const DIM_FG: Color = Color::Rgb(169, 177, 214);
/// Working.
pub const GREEN: Color = Color::Rgb(158, 206, 106);
/// Failed.
pub const RED: Color = Color::Rgb(247, 118, 142);
/// Inactive pane borders.
pub const BORDER_DIM: Color = Color::Rgb(59, 66, 97);
/// The deepest background (tmux popups and status line).
pub const BG_DEEP: Color = Color::Rgb(21, 22, 30);

/// tmux / fzf colour strings.
pub const ACCENT_HEX: &str = hex!(ACCENT);
pub const FG_HEX: &str = hex!(FG);
pub const DIM_FG_HEX: &str = hex!(DIM_FG);
pub const WORKING_HEX: &str = hex!(GREEN);
pub const BG_DEEP_HEX: &str = hex!(BG_DEEP);

/// Truecolour foreground escapes (`ESC[38;2;r;g;bm`).
pub const ACCENT_ANSI: &str = foreground_ansi!(ACCENT);
pub const WARN_ANSI: &str = foreground_ansi!(WARN);
pub const FG_ANSI: &str = foreground_ansi!(FG);
pub const DIM_FG_ANSI: &str = foreground_ansi!(DIM_FG);
pub const INFO_ANSI: &str = foreground_ansi!(INFO);

/// `color`, or its 256-colour palette entry when `TX_HOME_COLORS=256`.
pub fn palette(color: Color) -> Color {
    static INDEXED: OnceLock<bool> = OnceLock::new();
    let indexed_palette =
        INDEXED.get_or_init(|| std::env::var("TX_HOME_COLORS").is_ok_and(|value| value == "256"));
    if *indexed_palette {
        indexed(color)
    } else {
        color
    }
}

/// The nearest entry of the 6x6x6 colour cube; non-`Rgb` colours pass through.
pub fn indexed(color: Color) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let level = |channel: u8| (u16::from(channel) * 5 + 127) / 255;
    let index = 16 + 36 * level(red) + 6 * level(green) + level(blue);
    Color::Indexed(u8::try_from(index).expect("cube indices are at most 231"))
}

/// The longest escape: `ESC[38;2;255;255;255m`.
const ANSI_CAPACITY: usize = 19;

const fn channels(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(red, green, blue) => [red, green, blue],
        _ => panic!("palette colours are Rgb"),
    }
}

/// `#rrggbb`.
const fn hex_bytes(color: Color) -> [u8; 7] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let channels = channels(color);
    let mut bytes = [b'#'; 7];
    let mut index = 0;
    while index < 3 {
        bytes[1 + 2 * index] = DIGITS[(channels[index] >> 4) as usize];
        bytes[2 + 2 * index] = DIGITS[(channels[index] & 0xf) as usize];
        index += 1;
    }
    bytes
}

/// `ESC[38;2;r;g;bm` in a fixed buffer, and its length.
const fn ansi_bytes(color: Color) -> ([u8; ANSI_CAPACITY], usize) {
    let channels = channels(color);
    let mut bytes = [0; ANSI_CAPACITY];
    let prefix = b"\x1b[38;2";
    let mut length = 0;
    while length < prefix.len() {
        bytes[length] = prefix[length];
        length += 1;
    }
    let mut index = 0;
    while index < 3 {
        bytes[length] = b';';
        length += 1;
        let channel = channels[index];
        if channel >= 100 {
            bytes[length] = b'0' + channel / 100;
            length += 1;
        }
        if channel >= 10 {
            bytes[length] = b'0' + channel / 10 % 10;
            length += 1;
        }
        bytes[length] = b'0' + channel % 10;
        length += 1;
        index += 1;
    }
    bytes[length] = b'm';
    (bytes, length + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_strings_derive_from_the_constants() {
        assert_eq!(ACCENT_HEX, "#7aa2f7");
        assert_eq!(FG_HEX, "#c0caf5");
        assert_eq!(DIM_FG_HEX, "#a9b1d6");
        assert_eq!(WORKING_HEX, "#9ece6a");
        assert_eq!(BG_DEEP_HEX, "#15161e");
        assert_eq!(hex!(BORDER_DIM), "#3b4261");
    }

    #[test]
    fn ansi_escapes_derive_from_the_constants() {
        assert_eq!(WARN_ANSI, "\x1b[38;2;224;175;104m");
        assert_eq!(ACCENT_ANSI, "\x1b[38;2;122;162;247m");
        assert_eq!(FG_ANSI, "\x1b[38;2;192;202;245m");
        assert_eq!(DIM_FG_ANSI, "\x1b[38;2;169;177;214m");
        assert_eq!(INFO_ANSI, "\x1b[38;2;125;207;255m");
        assert_eq!(foreground_ansi!(Color::Rgb(0, 9, 10)), "\x1b[38;2;0;9;10m");
    }

    #[test]
    fn the_256_colour_palette_maps_rgb_into_the_cube_and_leaves_others() {
        assert_eq!(indexed(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(indexed(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(indexed(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(indexed(Color::Red), Color::Red);
    }
}
