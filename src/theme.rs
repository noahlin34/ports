use ratatui::style::{Color, Modifier, Style};

/// The visual language for the terminal UI. It intentionally uses a small
/// palette so the selected row, warnings, and exposure scope carry meaning.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub good: Color,
    pub warn: Color,
    pub danger: Color,
    pub external: Color,
    pub private: Color,
    pub local: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::Rgb(15, 17, 21),
            surface: Color::Rgb(24, 27, 33),
            border: Color::Rgb(59, 65, 77),
            text: Color::Rgb(224, 228, 235),
            muted: Color::Rgb(139, 148, 163),
            accent: Color::Rgb(121, 180, 255),
            accent_soft: Color::Rgb(53, 83, 119),
            good: Color::Rgb(113, 204, 150),
            warn: Color::Rgb(244, 188, 91),
            danger: Color::Rgb(245, 117, 117),
            external: Color::Rgb(241, 137, 101),
            private: Color::Rgb(218, 177, 103),
            local: Color::Rgb(113, 204, 150),
        }
    }
}

impl Theme {
    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }

    pub fn panel(self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    pub fn border(self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn title(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn selected(self) -> Style {
        Style::default()
            .fg(self.text)
            .bg(self.accent_soft)
            .add_modifier(Modifier::BOLD)
    }

    pub fn good(self) -> Style {
        Style::default().fg(self.good)
    }

    pub fn warning(self) -> Style {
        Style::default().fg(self.warn)
    }

    pub fn danger(self) -> Style {
        Style::default().fg(self.danger)
    }

    pub fn exposure(self, scope: ports::model::NetworkScope) -> Style {
        match scope {
            ports::model::NetworkScope::AllInterfaces | ports::model::NetworkScope::External => {
                Style::default().fg(self.external)
            }
            ports::model::NetworkScope::Private | ports::model::NetworkScope::Tailscale => {
                Style::default().fg(self.private)
            }
            ports::model::NetworkScope::LinkLocal | ports::model::NetworkScope::Loopback => {
                Style::default().fg(self.local)
            }
        }
    }

    pub fn hover_row(self) -> Style {
        Style::default().fg(self.text).bg(Color::Rgb(36, 42, 53))
    }

    pub fn hover_border(self) -> Style {
        Style::default().fg(self.accent_soft)
    }

    pub fn hover_button(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }

    pub fn hover_danger_button(self) -> Style {
        Style::default()
            .fg(self.danger)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }
}
