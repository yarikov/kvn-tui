use ratatui::style::{Modifier, Style};

use crate::ui::palette::{Palette, mix};

/// Palette-driven theme: every UI style is derived from a [`Palette`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    palette: Palette,
}

impl Default for Theme {
    /// The default theme reproduces the pre-theming look (see
    /// [`Palette::legacy`]).
    fn default() -> Self {
        Self::legacy()
    }
}

impl Theme {
    /// Build a theme from a raw palette.
    pub const fn from_palette(palette: Palette) -> Self {
        Self { palette }
    }

    /// The pre-theming fallback theme.
    pub const fn legacy() -> Self {
        Self::from_palette(Palette::legacy())
    }

    /// Look up an Omarchy theme by snake-case slug, falling back to
    /// [`Theme::legacy`] when the name is unknown or missing.
    pub fn resolve(name: &str) -> Self {
        Palette::lookup(name)
            .map(Self::from_palette)
            .unwrap_or_else(Self::legacy)
    }

    /// Primary accent color for highlights, hints, status badges.
    pub fn accent(&self) -> Style {
        Style::default()
            .fg(self.palette.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Default text style.
    pub fn normal(&self) -> Style {
        Style::default().fg(self.palette.foreground)
    }

    /// Style for status bar text (yellow / warning).
    pub fn status(&self) -> Style {
        Style::default().fg(self.palette.ansi[3])
    }

    /// Style for error messages.
    pub fn error(&self) -> Style {
        Style::default()
            .fg(self.palette.ansi[1])
            .add_modifier(Modifier::BOLD)
    }

    /// Style for success / connected state.
    pub fn success(&self) -> Style {
        Style::default()
            .fg(self.palette.ansi[2])
            .add_modifier(Modifier::BOLD)
    }

    /// Style for borders (dim).
    pub fn border(&self) -> Style {
        Style::default().fg(self.palette.ansi[8])
    }

    /// Style for a selected/hovered row (background highlight).
    pub fn selected(&self) -> Style {
        Style::default()
            .bg(self.palette.selection_background)
            .fg(self.palette.selection_foreground)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for a selected row that is also the active VPN connection.
    /// Background is a darkened blend of the palette's green and background.
    pub fn selected_connected(&self) -> Style {
        Style::default()
            .bg(mix(self.palette.ansi[2], self.palette.background, 0.75))
            .fg(self.palette.ansi[2])
            .add_modifier(Modifier::BOLD)
    }

    /// Style for help popup background.
    pub fn popup_bg(&self) -> Style {
        Style::default().bg(self.palette.background)
    }

    /// Style for the frame-wide background fill. Applied first by `draw()`
    /// so every subsequent widget that leaves `bg` unset (which is most of
    /// them — only popups override) inherits the palette's background
    /// instead of falling through to the terminal default.
    pub fn background(&self) -> Style {
        Style::default().bg(self.palette.background)
    }

    /// Raw palette background as a `Color` — for callers that need the
    /// value itself (e.g. emitting OSC 11 to color the terminal padding).
    pub fn palette_background(&self) -> ratatui::style::Color {
        self.palette.background
    }

    /// Raw palette foreground for synchronizing the terminal's default text
    /// color with cells that intentionally use `Style::default()`.
    pub fn palette_foreground(&self) -> ratatui::style::Color {
        self.palette.foreground
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn legacy_accent_is_cyan_bold() {
        let s = Theme::legacy().accent();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn legacy_normal_is_gray() {
        let s = Theme::legacy().normal();
        assert_eq!(s.fg, Some(Color::Gray));
    }

    #[test]
    fn legacy_status_is_yellow() {
        let s = Theme::legacy().status();
        assert_eq!(s.fg, Some(Color::Yellow));
    }

    #[test]
    fn legacy_error_is_red_bold() {
        let s = Theme::legacy().error();
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn legacy_success_is_green_bold() {
        let s = Theme::legacy().success();
        assert_eq!(s.fg, Some(Color::Green));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn legacy_border_is_dark_gray() {
        let s = Theme::legacy().border();
        assert_eq!(s.fg, Some(Color::DarkGray));
    }

    #[test]
    fn legacy_popup_bg_is_black() {
        let s = Theme::legacy().popup_bg();
        assert_eq!(s.bg, Some(Color::Black));
    }

    #[test]
    fn legacy_selected_matches_pre_theming_rgb() {
        let s = Theme::legacy().selected();
        assert_eq!(s.bg, Some(Color::Rgb(45, 45, 85)));
        assert_eq!(s.fg, Some(Color::Cyan));
    }

    #[test]
    fn default_theme_equals_legacy() {
        assert_eq!(Theme::default(), Theme::legacy());
    }

    #[test]
    fn resolve_known_theme_returns_omarchy_palette() {
        let t = Theme::resolve("tokyo-night");
        // Tokyo Night accent (#7aa2f7) should propagate into accent().
        assert_eq!(t.accent().fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    }

    #[test]
    fn resolve_unknown_theme_falls_back_to_legacy() {
        let t = Theme::resolve("not-a-real-theme");
        assert_eq!(t, Theme::legacy());
    }

    #[test]
    fn exposes_raw_palette_foreground() {
        let theme = Theme::resolve("catppuccin-latte");
        assert_eq!(theme.palette_foreground(), Color::Rgb(0x4c, 0x4f, 0x69));
    }
}
