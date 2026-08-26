use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::theme::Theme;

/// Draw the keyboard reference as a modal instead of replacing the underlying
/// screen. Keeping the overlay in its own module makes it difficult for a new
/// key binding to accidentally bypass modal trapping in `App::handle_key`.
pub fn render(frame: &mut Frame<'_>, area: Rect, theme: Theme) {
    let width = area.width.saturating_sub(8).clamp(54, 88);
    let height = area.height.saturating_sub(4).clamp(18, 30);
    let modal = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, modal);
    let heading = Line::from(vec![
        Span::styled(" ? ", theme.title()),
        Span::styled("Keyboard guide", theme.title()),
    ]);
    let rows = vec![
        Line::from(vec![Span::styled("Navigate", theme.title())]),
        key("↑ / k", "previous service", theme),
        key("↓ / j", "next service", theme),
        key("PgUp / PgDn", "move one page", theme),
        key("Home / End", "first / last service", theme),
        key("Tab", "overview · connections · inspection", theme),
        Line::from(""),
        Line::from(vec![Span::styled("Inspect", theme.title())]),
        key("Enter", "toggle the selected detail view", theme),
        key("p", "show the full binary path", theme),
        key("/", "search services (Esc cancels)", theme),
        key("r", "refresh discovery now", theme),
        key("c", "copy the raw bind endpoint", theme),
        key("u", "copy a conservative local HTTP URL", theme),
        key("o", "open a likely HTTP service", theme),
        Line::from(""),
        Line::from(vec![Span::styled("Manage", theme.title())]),
        key("x", "terminate process (SIGTERM with confirmation)", theme),
        key(
            "X",
            "force-kill process (SIGKILL with type-to-confirm)",
            theme,
        ),
        key("q", "quit", theme),
        Line::from(""),
        Line::from(vec![
            Span::styled("Esc", theme.muted()),
            Span::raw(" closes this guide or the active overlay"),
        ]),
    ];
    let text = Text::from(rows);
    let block = Block::default()
        .title(heading)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(theme.border())
        .style(theme.panel());
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(theme.text).bg(theme.surface))
            .wrap(Wrap { trim: false }),
        modal,
    );
}

fn key<'a>(binding: &'a str, meaning: &'a str, theme: Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{binding:<13}"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(meaning),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_rows_keep_the_binding_column_stable() {
        let line = key("x", "terminate", Theme::default());
        assert_eq!(line.spans[0].content, "x            ");
        assert_eq!(line.spans[1].content, "terminate");
    }
}
