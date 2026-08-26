use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::theme::Theme;

/// Draw the keyboard and mouse interaction reference as a modal instead of
/// replacing the underlying screen. Keeping the overlay in its own module
/// makes it difficult for a new binding to accidentally bypass modal trapping
/// in `App::handle_key`.
pub(crate) const CLOSE_LABEL: &str = "Esc closes";

pub fn render(frame: &mut Frame<'_>, area: Rect, theme: Theme) {
    let modal = modal_rect(area);

    frame.render_widget(Clear, modal);
    let heading = if modal.width >= 38 {
        Line::from(vec![
            Span::styled(" ? ", theme.title()),
            Span::styled("Keyboard + mouse guide", theme.title()),
        ])
    } else if modal.width >= 20 {
        Line::from(Span::styled(" ? Help", theme.title()))
    } else {
        Line::from("")
    };
    let rows = vec![
        key("Nav ↑↓/jk", "previous / next service", theme),
        key("Nav PgUp/Dn", "move one page", theme),
        key("Nav Home/End", "first / last service", theme),
        key("View Tab", "overview · connections · inspection", theme),
        key("Inspect Enter", "toggle selected detail", theme),
        key("Inspect /", "search services (Esc cancels)", theme),
        key(
            "Inspect r/o/p",
            "refresh / open HTTP / full binary path",
            theme,
        ),
        key("Inspect c/u", "copy endpoint / local URL", theme),
        key("Manage x", "terminate (SIGTERM; confirm)", theme),
        key("Manage X", "force-kill (SIGKILL; type KILL)", theme),
        key("Close q / Esc", "quit / close guide or overlay", theme),
        Line::from(vec![Span::styled("Mouse", theme.title())]),
        key("Move pointer", "highlight rows, panels, actions", theme),
        key("Click row", "select row + focus Overview", theme),
        key("Click panel", "focus that panel", theme),
        key("Wheel up/down", "move one row (↑ / ↓)", theme),
        key("Click action", "trigger keyboard equivalent", theme),
        key("Modal open", "modal controls; outside clicks no-op", theme),
    ];
    let text = Text::from(rows);
    let block = Block::default()
        .title_top(heading)
        .title_top(Line::from(Span::styled(CLOSE_LABEL, theme.title())).right_aligned())
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

/// Keep the modal inside the frame. `Clear` and paragraph rendering expect
/// their rectangles to be clipped before they index the backing buffer.
pub(crate) fn modal_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(8).clamp(54, 88).min(area.width);
    let height = area.height.saturating_sub(4).clamp(18, 30).min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
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
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn key_rows_keep_the_binding_column_stable() {
        let line = key("x", "terminate", Theme::default());
        assert_eq!(line.spans[0].content, "x            ");
        assert_eq!(line.spans[1].content, "terminate");
    }

    #[test]
    fn rendered_help_keeps_binding_column_at_supported_widths() {
        for (width, height) in [(54, 24), (80, 24), (88, 40)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    render(frame, area, Theme::default());
                })
                .unwrap();

            let area = terminal.backend().buffer().area;
            let modal = modal_rect(area);
            let lines = terminal
                .backend()
                .buffer()
                .content()
                .chunks(width as usize)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let expected_meaning_x = usize::from(modal.x + 1) + 13;

            for meaning in [
                "previous / next service",
                "select row + focus Overview",
                "focus that panel",
                "move one row (↑ / ↓)",
                "trigger keyboard equivalent",
                "modal controls; outside clicks no-op",
            ] {
                let meaning_x = lines
                    .iter()
                    .find_map(|line| {
                        line.find(meaning)
                            .map(|byte_index| line[..byte_index].chars().count())
                    })
                    .expect("mouse/help row should be rendered");
                assert_eq!(meaning_x, expected_meaning_x, "{meaning} shifted");
            }
        }
    }

    #[test]
    fn normal_height_shows_lower_actions_and_close_guidance() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, Theme::default());
            })
            .unwrap();

        let lines = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        for text in [
            "Nav ↑↓/jk",
            "Inspect Enter",
            "Mouse",
            "Inspect c/u",
            "Inspect r/o/p",
            "Manage x",
            "Manage X",
            "Close q / Esc",
            "Esc closes",
        ] {
            assert!(
                lines.iter().any(|line| line.contains(text)),
                "{text} should remain visible at 24 rows"
            );
        }
    }

    #[test]
    fn tiny_terminal_keeps_help_render_safe() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, Theme::default());
            })
            .unwrap();

        let top_line = terminal
            .backend()
            .buffer()
            .content()
            .chunks(20)
            .next()
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .unwrap();
        assert!(top_line.contains(CLOSE_LABEL));

        let modal = modal_rect(terminal.backend().buffer().area);
        assert!(modal.width <= 20);
        assert!(modal.height <= 4);
    }
}
