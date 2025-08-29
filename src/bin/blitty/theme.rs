use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug)]
struct Colors {
    bg: Color,
    fg: Color,
    comment: Color,
    cyan: Color,
    green: Color,
    pink: Color,
    purple: Color,
    red: Color,
    yellow: Color,
}

#[derive(Clone, Debug)]
struct ThemeData {
    colors: Colors,
}

impl ThemeData {
    fn dracula() -> Self {
        Self {
            colors: Colors {
                bg: Color::Rgb(40, 42, 54),
                fg: Color::Rgb(248, 248, 242),
                comment: Color::Rgb(98, 114, 164),
                cyan: Color::Rgb(139, 233, 253),
                green: Color::Rgb(80, 250, 123),
                pink: Color::Rgb(255, 121, 198),
                purple: Color::Rgb(189, 147, 249),
                red: Color::Rgb(255, 85, 85),
                yellow: Color::Rgb(241, 250, 140),
            },
        }
    }

    fn solarized_dark() -> Self {
        Self {
            colors: Colors {
                bg: Color::Rgb(0, 43, 54),
                fg: Color::Rgb(131, 148, 150),
                comment: Color::Rgb(101, 123, 131),
                cyan: Color::Rgb(42, 161, 152),
                green: Color::Rgb(133, 153, 0),
                pink: Color::Rgb(211, 54, 130),
                purple: Color::Rgb(108, 113, 196),
                red: Color::Rgb(220, 50, 47),
                yellow: Color::Rgb(181, 137, 0),
            },
        }
    }

    fn gruvbox() -> Self {
        Self {
            colors: Colors {
                bg: Color::Rgb(29, 32, 33),
                fg: Color::Rgb(235, 219, 178),
                comment: Color::Rgb(146, 131, 116),
                cyan: Color::Rgb(142, 192, 124),
                green: Color::Rgb(184, 187, 38),
                pink: Color::Rgb(251, 73, 52),
                purple: Color::Rgb(189, 174, 147),
                red: Color::Rgb(204, 36, 29),
                yellow: Color::Rgb(250, 189, 47),
            },
        }
    }
}

static mut CURRENT_THEME: ThemeData = ThemeData {
    colors: Colors {
        bg: Color::Rgb(40, 42, 54),
        fg: Color::Rgb(248, 248, 242),
        comment: Color::Rgb(98, 114, 164),
        cyan: Color::Rgb(139, 233, 253),
        green: Color::Rgb(80, 250, 123),
        pink: Color::Rgb(255, 121, 198),
        purple: Color::Rgb(189, 147, 249),
        red: Color::Rgb(255, 85, 85),
        yellow: Color::Rgb(241, 250, 140),
    },
};

pub fn set_theme(theme_name: &str) {
    unsafe {
        CURRENT_THEME = match theme_name {
            "Dracula" => ThemeData::dracula(),
            "SolarizedDark" => ThemeData::solarized_dark(),
            "Gruvbox" => ThemeData::gruvbox(),
            _ => ThemeData::dracula(),
        };
    }
}

#[allow(private_interfaces)]
pub(crate) fn get_current_colors() -> Colors {
    unsafe { CURRENT_THEME.colors }
}

pub struct Theme;

#[allow(non_snake_case)]
impl Theme {
    pub fn BG() -> Color {
        get_current_colors().bg
    }
    pub fn FG() -> Color {
        get_current_colors().fg
    }
    pub fn COMMENT() -> Color {
        get_current_colors().comment
    }
    pub fn CYAN() -> Color {
        get_current_colors().cyan
    }
    pub fn GREEN() -> Color {
        get_current_colors().green
    }
    pub fn PINK() -> Color {
        get_current_colors().pink
    }
    pub fn PURPLE() -> Color {
        get_current_colors().purple
    }
    pub fn RED() -> Color {
        get_current_colors().red
    }
    pub fn YELLOW() -> Color {
        get_current_colors().yellow
    }

    // header styling not used; removed

    pub fn dir() -> Style {
        Style::default().fg(Self::CYAN())
    }
    pub fn file() -> Style {
        Style::default().fg(Self::FG())
    }
    pub fn symlink() -> Style {
        Style::default().fg(Self::PURPLE())
    }
    pub fn selected() -> Style {
        Style::default()
            .fg(Self::PINK())
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }
    // status style not used; removed
}
