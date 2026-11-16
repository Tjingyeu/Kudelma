use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub accent: Color,
    pub user: Color,
    pub assistant: Color,
    pub dim: Color,
    pub error: Color,
    pub success: Color,
    pub warning: Color,
    pub code: Color,
    pub heading: Color,
    pub border: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            accent: Color::Cyan,
            user: Color::Green,
            assistant: Color::Reset,
            dim: Color::DarkGray,
            error: Color::Red,
            success: Color::Green,
            warning: Color::Yellow,
            code: Color::Yellow,
            heading: Color::Magenta,
            border: Color::DarkGray,
        }
    }

    pub fn light() -> Self {
        Self {
            accent: Color::Blue,
            user: Color::Green,
            assistant: Color::Reset,
            dim: Color::Gray,
            error: Color::Red,
            success: Color::Green,
            warning: Color::Rgb(160, 100, 0),
            code: Color::Rgb(120, 80, 0),
            heading: Color::Magenta,
            border: Color::Gray,
        }
    }

    pub fn for_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }
}
