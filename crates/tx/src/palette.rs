//! Colour palette (palette.py) — the mirror of `shared/palette.sh`, which stays canonical.
//! When the palette there changes, change these literals to match.

// ----- raw SGR escapes (the bash B / R / RFG) -----
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";
/// Default foreground only: keeps bold/bg, used after a coloured chip.
pub const RESET_FG: &str = "\x1b[39m";

// ----- semantic colours (tokyonight-night) -----
pub const ACCENT_HEX: &str = "#7aa2f7";
pub const ACCENT_ANSI: &str = "\x1b[38;2;122;162;247m";
pub const FG_HEX: &str = "#c0caf5";
pub const FG_ANSI: &str = "\x1b[38;2;192;202;245m";
pub const DIM_FG_HEX: &str = "#a9b1d6";
pub const DIM_FG_ANSI: &str = "\x1b[38;2;169;177;214m";
pub const WARN_HEX: &str = "#e0af68";
pub const WARN_ANSI: &str = "\x1b[38;2;224;175;104m";
pub const BG_DEEP_HEX: &str = "#15161e";
/// fzf `bg+` 256-colour index, matched to the curses TUIs.
pub const SELECTION_BG: &str = "236";

// ----- tag chip palette -----
pub const TAG_CUBE: [u8; 12] = [210, 167, 215, 149, 80, 73, 37, 117, 38, 141, 198, 140];

const HASH_MODULUS: u64 = 2_147_483_647;

/// Stable index into [`TAG_CUBE`]: a rolling hash over the Unicode code points (Q13 PARITY — the
/// bash helper agrees under a UTF-8 locale), mod the 31-bit prime, mod the palette size.
pub fn tag_color_index(tag: &str) -> usize {
    let hash = tag
        .chars()
        .fold(0u64, |acc, c| (acc * 31 + u64::from(c)) % HASH_MODULUS);
    // Lossless: the remainder is below TAG_CUBE.len().
    (hash % TAG_CUBE.len() as u64) as usize
}

/// The 256-colour cube value for `tag`.
pub fn tag_cube(tag: &str) -> u8 {
    TAG_CUBE[tag_color_index(tag)]
}

/// The SGR foreground escape colouring `tag`'s chip (`\e[38;5;Nm`).
pub fn tag_ansi(tag: &str) -> String {
    format!("\x1b[38;5;{}m", tag_cube(tag))
}

/// The tmux `colourN` name for `tag`.
pub fn tag_hex(tag: &str) -> String {
    format!("colour{}", tag_cube(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_hash_matches_python_reference() {
        let x200 = "x".repeat(200);
        let cases: [(&str, usize, u8); 11] = [
            ("", 0, 210),
            ("a", 1, 167),
            ("llm", 1, 167),
            ("nvim", 4, 80),
            ("shell", 8, 38),
            ("other", 0, 210),
            ("é", 5, 73),
            ("ab", 9, 141),
            ("日本語", 11, 140),
            (&x200, 0, 210),
            ("🙂tag", 1, 167),
        ];
        for (tag, index, cube) in cases {
            assert_eq!(tag_color_index(tag), index, "{tag:?}");
            assert_eq!(tag_cube(tag), cube, "{tag:?}");
        }
    }

    #[test]
    fn chip_formats() {
        assert_eq!(tag_ansi("é"), "\x1b[38;5;73m");
        assert_eq!(tag_hex("nvim"), "colour80");
    }
}
