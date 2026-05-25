use ratatui::style::{Color, Modifier, Style};

/// Color palette for the application.
pub struct Theme;

impl Theme {
    /// Primary accent color for selected items.
    pub fn accent() -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for the currently highlighted list item.
    pub fn selected() -> Style {
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    }

    /// Default text style.
    pub fn normal() -> Style {
        Style::default().fg(Color::Gray)
    }

    /// Style for status bar text.
    pub fn status() -> Style {
        Style::default().fg(Color::Yellow)
    }

    /// Style for error messages.
    pub fn error() -> Style {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }

    /// Style for success / connected state.
    pub fn success() -> Style {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for borders.
    pub fn border() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    /// Style for help popup background.
    pub fn popup_bg() -> Style {
        Style::default().bg(Color::Black)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_is_cyan_bold() {
        let s = Theme::accent();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selected_is_dark_gray_white_bold() {
        let s = Theme::selected();
        assert_eq!(s.bg, Some(Color::DarkGray));
        assert_eq!(s.fg, Some(Color::White));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn normal_is_gray() {
        let s = Theme::normal();
        assert_eq!(s.fg, Some(Color::Gray));
    }

    #[test]
    fn status_is_yellow() {
        let s = Theme::status();
        assert_eq!(s.fg, Some(Color::Yellow));
    }

    #[test]
    fn error_is_red_bold() {
        let s = Theme::error();
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn success_is_green_bold() {
        let s = Theme::success();
        assert_eq!(s.fg, Some(Color::Green));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn border_is_dark_gray() {
        let s = Theme::border();
        assert_eq!(s.fg, Some(Color::DarkGray));
    }

    #[test]
    fn popup_bg_is_black() {
        let s = Theme::popup_bg();
        assert_eq!(s.bg, Some(Color::Black));
    }
}
