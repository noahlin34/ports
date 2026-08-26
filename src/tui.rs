use std::io::{self, stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ports::model::{NetworkScope, ServiceRecord};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame, Terminal,
};

use crate::{
    app::{App, Focus, Overlay},
    help,
    theme::Theme,
};

const EVENT_POLL: Duration = Duration::from_millis(80);

/// Own terminal mode for the entire event loop. Drop is deliberately the only
/// restoration path, so an error or panic cannot strand the user's shell in
/// an alternate screen with raw input enabled.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

pub fn run() -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();
    terminal.clear()?;

    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(EVENT_POLL)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key)?;
            }
        }
        app.tick();
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let theme = Theme::default();
    let area = frame.area();
    frame.render_widget(Block::default().style(theme.base()), area);
    if area.width < 2 || area.height < 3 {
        return;
    }
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, root[0], app, theme);
    if area.width >= 112 {
        render_wide(frame, root[1], app, theme);
    } else {
        render_narrow(frame, root[1], app, theme);
    }
    render_footer(frame, root[2], app, theme);
    match &app.overlay {
        Overlay::Help => help::render(frame, area, theme),
        Overlay::Search => render_search_overlay(frame, area, app, theme),
        Overlay::BinaryPath(path) => render_binary_path(frame, area, path, theme),
        Overlay::Confirm(confirmation) => render_confirmation(frame, area, confirmation, theme),
        Overlay::None => {}
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let count = app.services.len();
    let visible = app.visible.len();
    let right = if app.search_query.is_empty() {
        format!("{visible}/{count} services")
    } else {
        format!("{visible}/{count} · /{}", app.search_query)
    };
    let title = Line::from(vec![
        Span::styled("PORTS", theme.title()),
        Span::styled("  local socket inspector", theme.muted()),
        Span::raw("                                                    "),
        Span::styled(right, theme.muted()),
    ]);
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(theme.border())
        .style(theme.panel());
    frame.render_widget(Paragraph::new(title).block(block), area);
}

fn render_wide(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(59), Constraint::Percentage(41)])
        .split(area);
    render_overview(frame, split[0], app, theme);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(5)])
        .split(split[1]);
    if app.focus == Focus::Inspection {
        render_inspection(frame, split[1], app, theme);
    } else {
        render_details(
            frame,
            right[0],
            app.selected_service(),
            app.focus == Focus::Connections,
            theme,
        );
        render_connections(
            frame,
            right[1],
            app.selected_service(),
            app.focus == Focus::Connections,
            theme,
        );
    }
}

fn render_narrow(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    if area.height < 15 {
        render_overview(frame, area, app, theme);
        return;
    }
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    render_overview(frame, split[0], app, theme);
    if app.focus == Focus::Inspection {
        render_inspection(frame, split[1], app, theme);
    } else {
        render_details(
            frame,
            split[1],
            app.selected_service(),
            app.focus == Focus::Connections,
            theme,
        );
    }
}

fn render_overview(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let title = if app.search_query.is_empty() {
        "Services".to_owned()
    } else {
        format!("Services · /{}", app.search_query)
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Overview {
            Style::default().fg(theme.accent)
        } else {
            theme.border()
        })
        .style(theme.panel());
    if app.visible.is_empty() {
        let message = if let Some(error) = &app.error {
            format!("No service rows\n\n{error}\n\nPress r to retry discovery")
        } else if app.services.is_empty() {
            "No listening services discovered.\n\nPress r to refresh.".to_owned()
        } else {
            "No services match this search.\n\nPress / to edit the query or Esc to close search."
                .to_owned()
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .style(theme.muted())
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let header = Row::new(vec!["", "BIND", "SCOPE", "PROCESS", "STATE", "PEERS"])
        .style(theme.muted())
        .height(1);
    let rows = app.visible.iter().map(|index| {
        let service = &app.services[*index];
        let process = if service.process.name.is_empty() {
            format!("PID {}", service.process.pid)
        } else {
            format!("{}  {}", service.process.name, service.process.pid)
        };
        Row::new(vec![
            Cell::from(service.protocol.as_str()),
            Cell::from(service.local.to_string()),
            Cell::from(scope_badge(service.scope)).style(theme.exposure(service.scope)),
            Cell::from(process),
            Cell::from(service.state.to_string()),
            Cell::from(service.connections.len().to_string()),
        ])
        .style(if service.state.is_listening() {
            theme.good()
        } else {
            Style::default().fg(theme.text)
        })
    });
    let mut state = TableState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Length(25),
                Constraint::Length(13),
                Constraint::Min(18),
                Constraint::Length(13),
                Constraint::Length(6),
            ],
        )
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(theme.selected())
        .highlight_symbol("▸ ")
        .style(Style::default().fg(theme.text)),
        area,
        &mut state,
    );
}

fn render_details(
    frame: &mut Frame<'_>,
    area: Rect,
    service: Option<&ServiceRecord>,
    connections_focus: bool,
    theme: Theme,
) {
    let block = Block::default()
        .title("Selected service")
        .borders(Borders::ALL)
        .border_style(if !connections_focus {
            Style::default().fg(theme.accent)
        } else {
            theme.border()
        })
        .style(theme.panel());
    let Some(service) = service else {
        frame.render_widget(
            Paragraph::new("Select a service to inspect process context.")
                .block(block)
                .style(theme.muted()),
            area,
        );
        return;
    };
    let command = service.process.command.as_deref().unwrap_or("—");
    let cwd = service
        .process
        .cwd
        .as_deref()
        .map_or_else(|| "—".to_owned(), |path| path.display().to_string());
    let executable = service
        .process
        .executable
        .as_deref()
        .map_or_else(|| "—".to_owned(), |path| path.display().to_string());
    let user = service.process.user.as_deref().unwrap_or("—");
    let bind = service.local.to_string();
    let state = service.state.to_string();
    let process = if service.process.name.is_empty() {
        format!("PID {}", service.process.pid)
    } else {
        format!("{} · PID {}", service.process.name, service.process.pid)
    };
    let lines = vec![
        detail_line("bind", &bind, theme.exposure(service.scope)),
        detail_line(
            "scope",
            service.scope.description(),
            theme.exposure(service.scope),
        ),
        detail_line("state", &state, theme.good()),
        detail_line("process", &process, Style::default().fg(theme.text)),
        detail_line("user", user, Style::default().fg(theme.text)),
        detail_line("cwd", &cwd, theme.muted()),
        detail_line("binary", &executable, theme.muted()),
        detail_line("command", command, theme.muted()),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_connections(
    frame: &mut Frame<'_>,
    area: Rect,
    service: Option<&ServiceRecord>,
    focused: bool,
    theme: Theme,
) {
    let block = Block::default()
        .title("Connections")
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(theme.accent)
        } else {
            theme.border()
        })
        .style(theme.panel());
    let Some(service) = service else {
        frame.render_widget(
            Paragraph::new("No selected service.")
                .block(block)
                .style(theme.muted()),
            area,
        );
        return;
    };
    if service.connections.is_empty() {
        frame.render_widget(
            Paragraph::new("No peer connections reported for this bind.")
                .block(block)
                .style(theme.muted()),
            area,
        );
        return;
    }
    let rows = service.connections.iter().map(|connection| {
        Row::new(vec![
            Cell::from(connection.protocol.as_str()),
            Cell::from(connection.remote.to_string()),
            Cell::from(connection.state.to_string()),
            Cell::from(scope_badge(connection.scope)).style(theme.exposure(connection.scope)),
            Cell::from(connection.process.pid.to_string()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Min(22),
                Constraint::Length(15),
                Constraint::Length(13),
                Constraint::Length(8),
            ],
        )
        .header(Row::new(vec!["", "PEER", "STATE", "SCOPE", "PID"]).style(theme.muted()))
        .block(block)
        .column_spacing(1),
        area,
    );
}

fn render_inspection(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let block = Block::default()
        .title("Inspection history · newest first")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .style(theme.panel());
    if app.history.is_empty() {
        frame.render_widget(
            Paragraph::new("Activity appears here as listeners open, close, or exit.")
                .block(block)
                .style(theme.muted()),
            area,
        );
        return;
    }
    let lines = app.history.iter().rev().map(|event| {
        let age = event.at.elapsed().as_secs();
        let age = if age == 0 {
            "now".to_owned()
        } else {
            format!("{age}s")
        };
        let style = match event.kind {
            crate::app::HistoryKind::Opened => theme.good(),
            crate::app::HistoryKind::Closed => theme.warning(),
            crate::app::HistoryKind::ProcessExited => theme.danger(),
        };
        Line::from(vec![
            Span::styled(format!("{:>4}  ", age), theme.muted()),
            Span::styled(
                format!("{}  ", event.symbol()),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::raw(&event.detail),
        ])
    });
    frame.render_widget(
        Paragraph::new(Text::from_iter(lines))
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let (left, left_style) = if let Some(error) = &app.error {
        (error.as_str(), theme.danger())
    } else if let Some(status) = &app.status {
        (status.as_str(), theme.muted())
    } else {
        ("ready · live discovery", theme.muted())
    };
    let refreshed = app.last_refresh.map_or_else(
        || "never".to_owned(),
        |instant| {
            let seconds = instant.elapsed().as_secs();
            if seconds == 0 {
                "just now".to_owned()
            } else {
                format!("{seconds}s ago")
            }
        },
    );
    let line = Line::from(vec![
        Span::styled(left, left_style),
        Span::raw("  "),
        Span::styled(format!("last refresh {refreshed}"), theme.muted()),
        Span::raw("                                      "),
        Span::styled("↑↓/jk", Style::default().fg(theme.accent)),
        Span::raw(" move  "),
        Span::styled("/", Style::default().fg(theme.accent)),
        Span::raw(" search  "),
        Span::styled("Tab", Style::default().fg(theme.accent)),
        Span::raw(" focus  "),
        Span::styled("p", Style::default().fg(theme.accent)),
        Span::raw(" path  "),
        Span::styled("x", Style::default().fg(theme.accent)),
        Span::raw(" kill  "),
        Span::styled("?", Style::default().fg(theme.accent)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(theme.accent)),
        Span::raw(" quit"),
    ]);
    frame.render_widget(
        Paragraph::new(line)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(theme.border()),
            )
            .style(theme.panel()),
        area,
    );
}

fn render_search_overlay(frame: &mut Frame<'_>, area: Rect, app: &App, theme: Theme) {
    let modal = centered(area, 70, 5);
    frame.render_widget(Clear, modal);
    let line = Line::from(vec![
        Span::styled(
            "/ ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(&app.search_input),
        Span::styled("▌", Style::default().fg(theme.accent)),
    ]);
    frame.render_widget(
        Paragraph::new(line)
            .block(
                Block::default()
                    .title("Search · Enter applies · Esc cancels")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent))
                    .style(theme.panel()),
            )
            .style(Style::default().fg(theme.text)),
        modal,
    );
}

fn render_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    confirmation: &crate::app::Confirmation,
    theme: Theme,
) {
    let modal_width = 72.min(area.width.saturating_sub(4)).max(20);
    let modal_height = 11.min(area.height.saturating_sub(2)).max(6);
    let modal = centered(area, modal_width, modal_height);
    frame.render_widget(Clear, modal);

    let (title, border_style) = if confirmation.is_blocked() {
        ("Blocked action · Esc to dismiss", theme.danger())
    } else {
        match confirmation.kind {
            crate::app::ConfirmKind::Terminate => ("Terminate process (SIGTERM)", theme.warning()),
            crate::app::ConfirmKind::Kill => ("Force-kill process (SIGKILL)", theme.danger()),
        }
    };

    let mut lines = Vec::new();

    // Process & user line
    let user = confirmation.process.user.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("Target:  ", theme.muted()),
        Span::styled(
            format!(
                "{} (PID {})",
                confirmation.process.name, confirmation.process.pid
            ),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("User: ", theme.muted()),
        Span::styled(
            user,
            if user == "root" {
                theme.warning()
            } else {
                Style::default().fg(theme.text)
            },
        ),
    ]));

    // Command line or binary path if present
    if let Some(command) = &confirmation.process.command {
        lines.push(Line::from(vec![
            Span::styled("Command: ", theme.muted()),
            Span::styled(command.as_str(), theme.muted()),
        ]));
    } else if let Some(executable) = &confirmation.process.executable {
        lines.push(Line::from(vec![
            Span::styled("Binary:  ", theme.muted()),
            Span::styled(executable.display().to_string(), theme.muted()),
        ]));
    }

    // Sockets affected
    let sockets_str = if confirmation.sockets.is_empty() {
        confirmation.endpoint.clone()
    } else {
        confirmation.sockets.join(", ")
    };
    lines.push(Line::from(vec![
        Span::styled("Sockets: ", theme.muted()),
        Span::styled(sockets_str, Style::default().fg(theme.accent)),
    ]));

    // Notes or warnings
    if let Some(reason) = &confirmation.blocked_reason {
        lines.push(Line::from(vec![
            Span::styled("Blocked: ", theme.danger().add_modifier(Modifier::BOLD)),
            Span::styled(reason.as_str(), theme.danger()),
        ]));
    } else if confirmation.connection_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("Warning: ", theme.warning().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(
                    "{} active peer connection(s) will be dropped.",
                    confirmation.connection_count
                ),
                theme.warning(),
            ),
        ]));
    } else if !confirmation.process.is_current_user && user == "root" {
        lines.push(Line::from(vec![
            Span::styled("Note: ", theme.warning().add_modifier(Modifier::BOLD)),
            Span::styled(
                "Process is owned by root and may require elevated privileges.",
                theme.warning(),
            ),
        ]));
    } else {
        lines.push(Line::from(""));
    }

    // Blank separator
    lines.push(Line::from(""));

    // Actions / prompt
    if confirmation.is_blocked() {
        lines.push(Line::from(vec![
            Span::styled(
                "Esc / Enter",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" close"),
        ]));
    } else {
        match confirmation.kind {
            crate::app::ConfirmKind::Terminate => {
                lines.push(Line::from(vec![
                    Span::styled("Enter / y", theme.danger().add_modifier(Modifier::BOLD)),
                    Span::raw(" terminate    "),
                    Span::styled("Esc / n", Style::default().fg(theme.accent)),
                    Span::raw(" cancel"),
                ]));
            }
            crate::app::ConfirmKind::Kill => {
                lines.push(Line::from(vec![
                    Span::styled("Type KILL to confirm: ", theme.muted()),
                    Span::styled(
                        format!("[ {}▌ ]", confirmation.input),
                        theme.danger().add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(
                        "Enter",
                        if confirmation.input.trim() == "KILL" {
                            theme.danger().add_modifier(Modifier::BOLD)
                        } else {
                            theme.muted()
                        },
                    ),
                    Span::raw(" force-kill (when KILL is typed)    "),
                    Span::styled("Esc", Style::default().fg(theme.accent)),
                    Span::raw(" cancel"),
                ]));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .style(theme.panel()),
            )
            .wrap(Wrap { trim: true }),
        modal,
    );
}

fn render_binary_path(frame: &mut Frame<'_>, area: Rect, path: &str, theme: Theme) {
    let modal = centered(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(path)
            .block(
                Block::default()
                    .title("Binary path · Esc closes")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent))
                    .style(theme.panel()),
            )
            .style(Style::default().fg(theme.text))
            .wrap(Wrap { trim: false }),
        modal,
    );
}

fn detail_line<'a>(label: &'a str, value: &'a str, value_style: Style) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<9}"), Theme::default().muted()),
        Span::styled(value, value_style),
    ])
}

fn scope_badge(scope: NetworkScope) -> &'static str {
    match scope {
        NetworkScope::AllInterfaces => "ALL",
        NetworkScope::External => "PUBLIC",
        NetworkScope::Private => "PRIVATE",
        NetworkScope::Tailscale => "TAILSCALE",
        NetworkScope::LinkLocal => "LINK-LOCAL",
        NetworkScope::Loopback => "LOCAL",
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ports::model::{Endpoint, ProcessMetadata, Protocol, ServiceRecord, SocketState};
    use ratatui::{backend::TestBackend, Terminal};
    use std::{net::IpAddr, path::PathBuf};

    #[test]
    fn narrow_and_wide_surfaces_render_without_panicking() {
        for (width, height) in [(80, 24), (140, 32), (40, 12)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render(frame, &App::default()))
                .unwrap();
        }
    }

    #[test]
    fn details_show_short_process_name_and_binary_field() {
        let mut process = ProcessMetadata::new(2710, "remoted");
        process.executable = Some(PathBuf::from(
            "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted",
        ));
        process.command = Some(
            "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted --flag"
                .into(),
        );
        let service = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8080),
            SocketState::Listening,
            process,
            None,
        );
        let app = App::from_services(vec![service]);
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                render_details(
                    frame,
                    frame.area(),
                    app.selected_service(),
                    false,
                    Theme::default(),
                )
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .collect::<String>();
        assert!(rendered.contains("remoted · PID 2710"));
        assert!(rendered.contains("binary"));
    }

    #[test]
    fn binary_path_overlay_renders_complete_long_path() {
        let mut process = ProcessMetadata::new(2710, "remoted");
        process.executable = Some(PathBuf::from(
            "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted",
        ));
        let service = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8080),
            SocketState::Listening,
            process,
            None,
        );
        let mut app = App::from_services(vec![service]);
        app.show_binary_path();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .collect::<String>();
        assert!(rendered.contains("Binary path"));
        assert!(rendered.contains(
            "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/r"
        ));
        assert!(rendered.contains("emoted"));
    }

    #[test]
    fn confirmation_overlay_renders_terminate_and_kill_states() {
        let mut process = ProcessMetadata::new(5432, "postgres");
        process.command = Some("/usr/local/bin/postgres -D /data".into());
        process.user = Some("postgres".into());
        let service = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 5432),
            SocketState::Listening,
            process,
            None,
        );

        // Test Terminate overlay rendering
        let mut app = App::from_services(vec![service.clone()]);
        app.request_confirmation(crate::app::ConfirmKind::Terminate);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .collect::<String>();
        assert!(rendered.contains("Terminate process (SIGTERM)"));
        assert!(rendered.contains("postgres (PID 5432)"));
        assert!(rendered.contains("User: postgres"));
        assert!(rendered.contains("Command: /usr/local/bin/postgres"));
        assert!(rendered.contains("Enter / y"));
        assert!(rendered.contains("terminate"));

        // Test Force-Kill overlay rendering
        let mut app_kill = App::from_services(vec![service]);
        app_kill.request_confirmation(crate::app::ConfirmKind::Kill);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app_kill)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .collect::<String>();
        assert!(rendered.contains("Force-kill process (SIGKILL)"));
        assert!(rendered.contains("Type KILL to confirm"));

        // Test Blocked system process overlay rendering
        let root_service = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 80),
            SocketState::Listening,
            ProcessMetadata::new(1, "launchd"),
            None,
        );
        let mut app_blocked = App::from_services(vec![root_service]);
        app_blocked.request_confirmation(crate::app::ConfirmKind::Terminate);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app_blocked)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .collect::<String>();
        assert!(rendered.contains("Blocked"));
        assert!(rendered.contains("launchd (PID 1)"));
    }
}
