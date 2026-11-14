//! The tx-ide palette (Tokyo Night), defined once. The RGB constants are the source. Next to
//! each sit the forms other surfaces use, written out by hand: the `_HEX` strings for tmux and
//! fzf, the `_ANSI` truecolour foreground escapes (`ESC[38;2;r;g;bm`); a test checks them
//! against the constants. `tx-client/tmux/keys.tmux` is shell and repeats two hex values.
//!
//! Every ratatui surface routes its colours through [`palette`], so `TX_HOME_COLORS=256` swaps
//! them for the nearest entries of the 256-colour palette.

use std::sync::OnceLock;

use ratatui_core::style::Color;

/// "This is tx" / "this is selected": the active pane border, headers, the selected row.
pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const ACCENT_HEX: &str = "#7aa2f7";
pub const ACCENT_ANSI: &str = "\x1b[38;2;122;162;247m";

/// Text on an accent background.
pub const ON_ACCENT: Color = Color::Rgb(26, 27, 38);

pub const INFO: Color = Color::Rgb(125, 207, 255);
pub const INFO_ANSI: &str = "\x1b[38;2;125;207;255m";

/// In transition, look at this.
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const WARN_ANSI: &str = "\x1b[38;2;224;175;104m";

/// Primary text.
pub const FG: Color = Color::Rgb(192, 202, 245);
pub const FG_HEX: &str = "#c0caf5";
pub const FG_ANSI: &str = "\x1b[38;2;192;202;245m";

/// Secondary text, always with the DIM modifier.
pub const DIM_FG: Color = Color::Rgb(169, 177, 214);
pub const DIM_FG_HEX: &str = "#a9b1d6";
pub const DIM_FG_ANSI: &str = "\x1b[38;2;169;177;214m";

/// Working.
pub const GREEN: Color = Color::Rgb(158, 206, 106);
pub const WORKING_HEX: &str = "#9ece6a";

/// Failed.
pub const RED: Color = Color::Rgb(247, 118, 142);

/// Inactive pane borders.
pub const BORDER_DIM: Color = Color::Rgb(59, 66, 97);

/// The deepest background (tmux popups and status line).
pub const BG_DEEP: Color = Color::Rgb(21, 22, 30);
pub const BG_DEEP_HEX: &str = "#15161e";

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

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(color: Color) -> (u8, u8, u8) {
        let Color::Rgb(red, green, blue) = color else {
            panic!("palette colours are Rgb");
        };
        (red, green, blue)
    }

    #[test]
    fn hex_and_ansi_strings_match_the_rgb_constants() {
        let hex = |color| {
            let (red, green, blue) = channels(color);
            format!("#{red:02x}{green:02x}{blue:02x}")
        };
        let ansi = |color| {
            let (red, green, blue) = channels(color);
            format!("\x1b[38;2;{red};{green};{blue}m")
        };
        for (text, color) in [
            (ACCENT_HEX, ACCENT),
            (FG_HEX, FG),
            (DIM_FG_HEX, DIM_FG),
            (WORKING_HEX, GREEN),
            (BG_DEEP_HEX, BG_DEEP),
        ] {
            assert_eq!(text, hex(color));
        }
        for (text, color) in [
            (ACCENT_ANSI, ACCENT),
            (WARN_ANSI, WARN),
            (FG_ANSI, FG),
            (DIM_FG_ANSI, DIM_FG),
            (INFO_ANSI, INFO),
        ] {
            assert_eq!(text, ansi(color));
        }
    }

    #[test]
    fn the_256_colour_palette_maps_rgb_into_the_cube_and_leaves_others() {
        assert_eq!(indexed(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(indexed(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(indexed(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(indexed(Color::Red), Color::Red);
    }
}
