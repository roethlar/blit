
use ratatui::style::{Color, Style, Modifier};

pub struct Theme;

impl Theme {
    // Dracula palette
    pub const BG: Color = Color::Rgb(40, 42, 54);
    pub const FG: Color = Color::Rgb(248, 248, 242);
    pub const COMMENT: Color = Color::Rgb(98, 114, 164);
    pub const CYAN: Color = Color::Rgb(139, 233, 253);
    pub const GREEN: Color = Color::Rgb(80, 250, 123);
    pub const ORANGE: Color = Color::Rgb(255, 184, 108);
    pub const PINK: Color = Color::Rgb(255, 121, 198);
    pub const PURPLE: Color = Color::Rgb(189, 147, 249);
    pub const RED: Color = Color::Rgb(255, 85, 85);
    pub const YELLOW: Color = Color::Rgb(241, 250, 140);

    pub fn header(focused: bool) -> Style {
        if focused {
            Style::default().fg(Self::FG).bg(Self::BG).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(Self::FG).bg(Self::BG).add_modifier(Modifier::BOLD)
        }
    }

    pub fn dir() -> Style { Style::default().fg(Self::CYAN) }
    pub fn file() -> Style { Style::default().fg(Self::FG) }
    pub fn symlink() -> Style { Style::default().fg(Self::PURPLE) }
    pub fn selected() -> Style { Style::default().fg(Self::PINK).add_modifier(Modifier::REVERSED | Modifier::BOLD) }
    pub fn status() -> Style { Style::default().fg(Self::FG) }
}
