//! Color palettes that drive every UI style.
//!
//! At build time `build.rs` parses `themes/*.toml` and emits a static
//! `BUNDLED` table — see `bundled_palettes.rs` in `OUT_DIR`. At runtime the
//! The TUI client reads Omarchy's active `colors.toml` in auto-follow mode;
//! explicitly selected themes continue to resolve against the bundled table.
//! [`Palette::legacy`] reproduces the original hardcoded look of kvn-tui.

use ratatui::style::Color;
use serde::Deserialize;

#[derive(Deserialize)]
struct OmarchyColors {
    accent: String,
    selection: String,
    muted: String,
    foreground: String,
    background: String,
    bright_foreground: String,
    red: String,
    yellow: String,
    green: String,
    cyan: String,
    blue: String,
    magenta: String,
    bright_red: String,
    bright_yellow: String,
    bright_green: String,
    bright_cyan: String,
    bright_blue: String,
    bright_magenta: String,
}

/// A self-contained color palette derived at build time from Omarchy 4's
/// semantic `colors.toml` schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,
    pub cursor: Color,
    pub foreground: Color,
    pub background: Color,
    pub selection_foreground: Color,
    pub selection_background: Color,
    pub ansi: [Color; 16],
}

include!(concat!(env!("OUT_DIR"), "/bundled_palettes.rs"));

impl Palette {
    /// Parse Omarchy's semantic `colors.toml` format into a runtime palette.
    /// Extra fields are accepted so custom themes may use the complete
    /// Omarchy schema while kvn-tui consumes only the colors it needs.
    pub fn from_omarchy_toml(raw: &str) -> Result<Palette, String> {
        let colors: OmarchyColors = toml::from_str(raw).map_err(|error| error.to_string())?;
        let parse = |value: &str| parse_hex_color(value);

        Ok(Palette {
            accent: parse(&colors.accent)?,
            cursor: parse(&colors.bright_foreground)?,
            foreground: parse(&colors.foreground)?,
            background: parse(&colors.background)?,
            selection_foreground: parse(&colors.bright_foreground)?,
            selection_background: parse(&colors.selection)?,
            ansi: [
                parse(&colors.background)?,
                parse(&colors.red)?,
                parse(&colors.green)?,
                parse(&colors.yellow)?,
                parse(&colors.blue)?,
                parse(&colors.magenta)?,
                parse(&colors.cyan)?,
                parse(&colors.foreground)?,
                parse(&colors.muted)?,
                parse(&colors.bright_red)?,
                parse(&colors.bright_green)?,
                parse(&colors.bright_yellow)?,
                parse(&colors.bright_blue)?,
                parse(&colors.bright_magenta)?,
                parse(&colors.bright_cyan)?,
                parse(&colors.bright_foreground)?,
            ],
        })
    }

    /// Look up a bundled palette by Omarchy theme name (snake-case slug from
    /// active `theme.name`).
    pub fn lookup(name: &str) -> Option<Palette> {
        BUNDLED.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
    }

    /// Names of every bundled palette, sorted alphabetically. Used by the
    /// in-TUI theme picker to build its menu.
    pub fn bundled_names() -> Vec<&'static str> {
        BUNDLED.iter().map(|(n, _)| *n).collect()
    }

    /// Fallback palette that reproduces the pre-theming look: cyan accents,
    /// gray text, named ANSI colors throughout.
    pub const fn legacy() -> Palette {
        Palette {
            accent: Color::Cyan,
            cursor: Color::White,
            foreground: Color::Gray,
            background: Color::Black,
            selection_foreground: Color::Cyan,
            selection_background: Color::Rgb(45, 45, 85),
            ansi: [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::White,
                Color::DarkGray,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
                Color::White,
            ],
        }
    }
}

fn parse_hex_color(value: &str) -> Result<Color, String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("expected #RRGGBB color, got {value:?}"));
    }
    let component = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).map_err(|error| error.to_string())
    };
    Ok(Color::Rgb(
        component(0..2)?,
        component(2..4)?,
        component(4..6)?,
    ))
}

/// Linear interpolation between two colors in sRGB space. Used to derive a
/// "darkened" highlight (the active-connection row background) from the
/// palette's green and background. `t` is the weight of `b` in the result.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = to_rgb(a);
    let (br, bg, bb) = to_rgb(b);
    let lerp = |x: u8, y: u8| {
        let v = f32::from(x) * (1.0 - t) + f32::from(y) * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

/// Best-effort RGB resolution for a `ratatui` `Color`. Named ANSI colors use
/// the conventional VGA-ish triples; non-RGB exotic variants fall through to
/// black, which is fine because [`mix`] and the OSC 11 emitter only operate
/// on palette colors that are either RGB or familiar ANSI names.
pub fn to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (170, 0, 0),
        Color::Green => (0, 170, 0),
        Color::Yellow => (170, 85, 0),
        Color::Blue => (0, 0, 170),
        Color::Magenta => (170, 0, 170),
        Color::Cyan => (0, 170, 170),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (85, 85, 85),
        Color::LightRed => (255, 85, 85),
        Color::LightGreen => (85, 255, 85),
        Color::LightYellow => (255, 255, 85),
        Color::LightBlue => (85, 85, 255),
        Color::LightMagenta => (255, 85, 255),
        Color::LightCyan => (85, 255, 255),
        Color::White => (255, 255, 255),
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_all_22_omarchy_themes() {
        let names = Palette::bundled_names();
        let expected = [
            "catppuccin",
            "catppuccin-latte",
            "ethereal",
            "everforest",
            "flexoki-light",
            "gruvbox",
            "hackerman",
            "kanagawa",
            "last-horizon",
            "lumon",
            "lupine",
            "matte-black",
            "miasma",
            "nord",
            "osaka-jade",
            "retro-82",
            "ristretto",
            "rose-pine",
            "solitude",
            "tokyo-night",
            "vantablack",
            "white",
        ];
        for name in expected {
            assert!(names.contains(&name), "missing palette: {name}");
        }
    }

    #[test]
    fn lookup_returns_known_gruvbox_palette() {
        let p = Palette::lookup("gruvbox").expect("gruvbox");
        // Spot-check accent — matches themes/gruvbox.toml.
        assert_eq!(p.accent, Color::Rgb(0x7d, 0xae, 0xa3));
        // color1 (red) used by error().
        assert_eq!(p.ansi[1], Color::Rgb(0xea, 0x69, 0x62));
    }

    #[test]
    fn lookup_unknown_theme_returns_none() {
        assert!(Palette::lookup("does-not-exist").is_none());
    }

    #[test]
    fn bundled_foregrounds_contrast_with_their_backgrounds() {
        fn channel(value: u8) -> f64 {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(color: Color) -> f64 {
            let (r, g, b) = to_rgb(color);
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        }

        for (name, palette) in BUNDLED {
            let foreground = luminance(palette.foreground);
            let background = luminance(palette.background);
            let ratio = (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05);
            assert!(
                ratio >= 3.0,
                "{name} foreground contrast is only {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn parses_runtime_omarchy_palette() {
        let raw = include_str!("../../themes/tokyo-night.toml")
            .replace("#7aa2f7", "#010203")
            .replace("#1a1b26", "#040506");
        let palette = Palette::from_omarchy_toml(&raw).unwrap();
        assert_eq!(palette.accent, Color::Rgb(1, 2, 3));
        assert_eq!(palette.background, Color::Rgb(4, 5, 6));
        assert_eq!(palette.ansi[0], Color::Rgb(4, 5, 6));
    }

    #[test]
    fn rejects_incomplete_or_invalid_runtime_palette() {
        assert!(Palette::from_omarchy_toml("accent = '#ffffff'").is_err());
        let invalid =
            include_str!("../../themes/tokyo-night.toml").replace("#7aa2f7", "not-a-color");
        assert!(Palette::from_omarchy_toml(&invalid).is_err());
    }

    #[test]
    fn legacy_palette_preserves_pre_theming_look() {
        let p = Palette::legacy();
        assert_eq!(p.accent, Color::Cyan);
        assert_eq!(p.foreground, Color::Gray);
        assert_eq!(p.ansi[1], Color::Red);
        assert_eq!(p.ansi[2], Color::Green);
        assert_eq!(p.ansi[3], Color::Yellow);
        assert_eq!(p.ansi[8], Color::DarkGray);
        assert_eq!(p.selection_background, Color::Rgb(45, 45, 85));
    }

    #[test]
    fn mix_clamps_endpoints() {
        let a = Color::Rgb(100, 200, 0);
        let b = Color::Rgb(0, 0, 100);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
    }

    #[test]
    fn mix_blends_named_colors_via_rgb_table() {
        // Green=(0,170,0), Black=(0,0,0); 75% toward black ≈ (0, 43, 0).
        let blended = mix(Color::Green, Color::Black, 0.75);
        assert_eq!(blended, Color::Rgb(0, 43, 0));
    }
}
