use std::io::{self, stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, MouseButton, MouseEvent,
        MouseEventKind,
    },
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
    app::{App, ConfirmKind, Focus, Overlay},
    help,
    theme::Theme,
};

const EVENT_POLL: Duration = Duration::from_millis(80);

/// Own terminal mode for the entire event loop. Drop is deliberately the only
/// restoration path, so an error or panic cannot strand the user's shell in
/// an alternate screen with raw input enabled or mouse tracking on.
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Identifies all responsive, interactive UI elements in the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HitTarget {
    OverviewRow(usize),
    OverviewPanel,
    DetailsPanel,
    ConnectionsPanel,
    InspectionPanel,
    FooterSearch,
    FooterFocus,
    FooterPath,
    FooterKill,
    FooterHelp,
    FooterQuit,
    SearchApply,
    SearchCancel,
    HelpClose,
    BinaryPathClose,
    ConfirmExecute,
    ConfirmCancel,
    ConfirmDismiss,
    ModalBackdrop,
}

/// Transient hover presentation state. Hover is strictly visual and never
/// mutates App domain state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HoverState {
    pub target: Option<HitTarget>,
}

/// A spatial index of clickable/scrollable regions derived pure from rendered geometry.
#[derive(Clone, Debug, Default)]
pub struct HitMap {
    entries: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, rect: Rect, target: HitTarget) {
        if rect.width > 0 && rect.height > 0 {
            self.entries.push((rect, target));
        }
    }

    /// Find the topmost hit target matching the provided column and row.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<HitTarget> {
        for (rect, target) in self.entries.iter().rev() {
            if col >= rect.x
                && col < rect.x.saturating_add(rect.width)
                && row >= rect.y
                && row < rect.y.saturating_add(rect.height)
            {
                return Some(*target);
            }
        }
        None
    }
}

/// Calculates the starting visible row offset in the overview table.
pub fn calculate_table_offset(selected: usize, total_items: usize, available_rows: usize) -> usize {
    if total_items == 0 || available_rows == 0 {
        return 0;
    }
    let max_offset = total_items.saturating_sub(available_rows);
    let selected = selected.min(total_items.saturating_sub(1));
    if selected >= available_rows {
        (selected + 1 - available_rows).min(max_offset)
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug)]
struct FooterControl {
    target: HitTarget,
    key: &'static str,
    label: &'static str,
    x: u16,
    width: u16,
}

#[derive(Clone, Debug)]
struct FooterLayout {
    left: String,
    refreshed: String,
    controls: [FooterControl; 6],
}

fn text_width(text: &str) -> u16 {
    Span::raw(text).width().min(u16::MAX as usize) as u16
}

fn footer_layout(area: Rect, app: &App) -> FooterLayout {
    let left = if let Some(error) = &app.error {
        error.clone()
    } else if let Some(status) = &app.status {
        status.clone()
    } else {
        "ready · live discovery".to_owned()
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
    let prefix_width = text_width(&left)
        .saturating_add(text_width("  "))
        .saturating_add(text_width(&format!("last refresh {refreshed}")))
        .saturating_add(text_width("                                      "))
        .saturating_add(text_width("↑↓/jk"))
        .saturating_add(text_width(" move  "));

    let definitions = [
        (HitTarget::FooterSearch, "/", " search"),
        (HitTarget::FooterFocus, "Tab", " focus"),
        (HitTarget::FooterPath, "p", " path"),
        (HitTarget::FooterKill, "x", " kill"),
        (HitTarget::FooterHelp, "?", " help"),
        (HitTarget::FooterQuit, "q", " quit"),
    ];
    let mut current_x = area.x.saturating_add(prefix_width);
    let controls = definitions.map(|(target, key, label)| {
        let width = text_width(key).saturating_add(text_width(label));
        let control = FooterControl {
            target,
            key,
            label,
            x: current_x,
            width,
        };
        current_x = current_x.saturating_add(width).saturating_add(2);
        control
    });

    FooterLayout {
        left,
        refreshed,
        controls,
    }
}

fn add_if_fully_visible(hit_map: &mut HitMap, bounds: Rect, rect: Rect, target: HitTarget) {
    let right = rect.x.saturating_add(rect.width);
    let bottom = rect.y.saturating_add(rect.height);
    if rect.width > 0
        && rect.height > 0
        && rect.x >= bounds.x
        && rect.y >= bounds.y
        && right <= bounds.x.saturating_add(bounds.width)
        && bottom <= bounds.y.saturating_add(bounds.height)
    {
        hit_map.add(rect, target);
    }
}

fn modal_inner(modal: Rect) -> Rect {
    Rect {
        x: modal.x.saturating_add(1),
        y: modal.y.saturating_add(1),
        width: modal.width.saturating_sub(2),
        height: modal.height.saturating_sub(2),
    }
}

fn search_modal(area: Rect) -> Rect {
    centered(area, 70, 5)
}

fn search_controls(area: Rect) -> (Rect, Rect, Rect) {
    let modal = search_modal(area);
    let content = modal_inner(modal);
    let action_y = modal.y.saturating_add(3);
    let apply_width = text_width("Enter applies");
    let cancel_x = content
        .x
        .saturating_add(apply_width)
        .saturating_add(text_width("    "));
    let cancel_width = text_width("Esc cancels");
    (
        modal,
        Rect::new(content.x, action_y, apply_width, 1),
        Rect::new(cancel_x, action_y, cancel_width, 1),
    )
}

fn binary_path_modal(area: Rect) -> Rect {
    centered(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    )
}

fn binary_path_close_rect(area: Rect) -> (Rect, Rect) {
    let modal = binary_path_modal(area);
    let content = modal_inner(modal);
    let close = Rect::new(
        content.x.saturating_add(text_width("Binary path · ")),
        modal.y,
        text_width("Esc closes"),
        1,
    );
    (modal, close)
}
#[derive(Clone, Copy, Debug)]
struct ConfirmationControlSpec {
    target: HitTarget,
    key: &'static str,
    label: &'static str,
}

#[derive(Clone, Debug)]
struct ConfirmationActionLayout {
    modal: Rect,
    bounds: Rect,
    controls: Vec<(Rect, HitTarget)>,
    rows: Vec<Vec<ConfirmationControlSpec>>,
    kill_input_row: bool,
}

fn control_row_text(row: &[ConfirmationControlSpec]) -> String {
    row.iter()
        .enumerate()
        .map(|(index, control)| {
            let gap = if index + 1 == row.len() { "" } else { "    " };
            format!("{}{}{}", control.key, control.label, gap)
        })
        .collect()
}

fn wrapped_line_height_from_width(rendered_width: usize, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let width = width as usize;
    (rendered_width.max(1).saturating_add(width - 1) / width).min(u16::MAX as usize) as u16
}

fn wrapped_line_height(text: &str, width: u16) -> u16 {
    wrapped_line_height_from_width(text_width(text) as usize, width)
}

fn wrapped_text_height(lines: &[Line<'_>], width: u16) -> u16 {
    lines.iter().fold(0, |height, line| {
        height.saturating_add(wrapped_line_height_from_width(line.width(), width))
    })
}

fn confirmation_action_layout(
    area: Rect,
    confirmation: &crate::app::Confirmation,
) -> ConfirmationActionLayout {
    let modal_width = 72.min(area.width.saturating_sub(4)).max(20).min(area.width);
    let modal_height = 11
        .min(area.height.saturating_sub(2))
        .max(6)
        .min(area.height);
    let modal = centered(area, modal_width, modal_height);
    let content = modal_inner(modal);
    let body_lines = confirmation_body_lines(confirmation, Theme::default());
    let body_height = wrapped_text_height(&body_lines, content.width);
    let action_y = content.y.saturating_add(body_height);

    let terminate = [
        ConfirmationControlSpec {
            target: HitTarget::ConfirmExecute,
            key: "Enter / y",
            label: " terminate",
        },
        ConfirmationControlSpec {
            target: HitTarget::ConfirmCancel,
            key: "Esc / n",
            label: " cancel",
        },
    ];
    let kill = [
        ConfirmationControlSpec {
            target: HitTarget::ConfirmExecute,
            key: "Enter",
            label: " force-kill (when KILL is typed)",
        },
        ConfirmationControlSpec {
            target: HitTarget::ConfirmCancel,
            key: "Esc",
            label: " cancel",
        },
    ];
    let dismiss = [ConfirmationControlSpec {
        target: HitTarget::ConfirmDismiss,
        key: "Esc / Enter",
        label: " close",
    }];

    let (rows, kill_input_row) = if confirmation.is_blocked() {
        (vec![dismiss.to_vec()], false)
    } else {
        match confirmation.kind {
            ConfirmKind::Terminate => {
                if text_width(&control_row_text(&terminate)) <= content.width {
                    (vec![terminate.to_vec()], false)
                } else {
                    (
                        terminate.iter().map(|control| vec![*control]).collect(),
                        false,
                    )
                }
            }
            ConfirmKind::Kill => {
                if text_width(&control_row_text(&kill)) <= content.width {
                    (vec![kill.to_vec()], true)
                } else {
                    (kill.iter().map(|control| vec![*control]).collect(), true)
                }
            }
        }
    };

    let mut controls = Vec::new();
    let mut row_y = action_y;
    if kill_input_row {
        let input = format!("Type KILL to confirm: [ {}▌ ]", confirmation.input);
        row_y = row_y.saturating_add(wrapped_line_height(&input, content.width));
    }
    for row in &rows {
        let row_text = control_row_text(row);
        let row_width = text_width(&row_text);
        let row_height = wrapped_line_height(&row_text, content.width);
        if row_width <= content.width && row_height == 1 {
            let mut x = content.x;
            for (index, control) in row.iter().enumerate() {
                let width = text_width(control.key).saturating_add(text_width(control.label));
                controls.push((Rect::new(x, row_y, width, 1), control.target));
                x = x
                    .saturating_add(width)
                    .saturating_add(if index + 1 == row.len() { 0 } else { 4 });
            }
        }
        row_y = row_y.saturating_add(row_height);
    }

    ConfirmationActionLayout {
        modal,
        bounds: content,
        controls,
        rows,
        kill_input_row,
    }
}

/// Build a fresh pure HitMap from exactly the responsive Rects rendered this frame.
pub fn build_hit_map(area: Rect, app: &App) -> HitMap {
    let mut hit_map = HitMap::new();

    if area.width < 2 || area.height < 3 {
        return hit_map;
    }

    // When an overlay is active, the overlay traps all events completely.
    // Underlying regions are never registered in the HitMap.
    match &app.overlay {
        Overlay::Search => {
            hit_map.add(area, HitTarget::ModalBackdrop);
            let (modal, apply_rect, cancel_rect) = search_controls(area);
            let content = modal_inner(modal);
            add_if_fully_visible(&mut hit_map, content, apply_rect, HitTarget::SearchApply);
            add_if_fully_visible(&mut hit_map, content, cancel_rect, HitTarget::SearchCancel);
            return hit_map;
        }
        Overlay::Help => {
            hit_map.add(area, HitTarget::ModalBackdrop);
            let modal = help::modal_rect(area);
            let close_width = text_width(help::CLOSE_LABEL);
            let close_rect = Rect::new(
                modal
                    .x
                    .saturating_add(modal.width.saturating_sub(1))
                    .saturating_sub(close_width),
                modal.y,
                close_width,
                1,
            );
            if modal.width >= close_width.saturating_add(2) {
                add_if_fully_visible(
                    &mut hit_map,
                    Rect::new(modal.x, modal.y, modal.width, 1),
                    close_rect,
                    HitTarget::HelpClose,
                );
            }
            return hit_map;
        }
        Overlay::BinaryPath(_) => {
            hit_map.add(area, HitTarget::ModalBackdrop);
            let (modal, close_rect) = binary_path_close_rect(area);
            add_if_fully_visible(
                &mut hit_map,
                Rect::new(
                    modal.x.saturating_add(1),
                    modal.y,
                    modal.width.saturating_sub(2),
                    1,
                ),
                close_rect,
                HitTarget::BinaryPathClose,
            );
            return hit_map;
        }
        Overlay::Confirm(confirmation) => {
            hit_map.add(area, HitTarget::ModalBackdrop);
            let layout = confirmation_action_layout(area, confirmation);
            for (rect, target) in layout.controls {
                add_if_fully_visible(&mut hit_map, layout.bounds, rect, target);
            }
            return hit_map;
        }
        Overlay::None => {}
    }

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    let body = root[1];
    let (overview_area, details_area, connections_area, inspection_area) = if area.width >= 112 {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(59), Constraint::Percentage(41)])
            .split(body);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(13), Constraint::Min(5)])
            .split(split[1]);

        if app.focus == Focus::Inspection {
            (split[0], None, None, Some(split[1]))
        } else {
            (split[0], Some(right[0]), Some(right[1]), None)
        }
    } else if body.height < 15 {
        (body, None, None, None)
    } else {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(body);

        if app.focus == Focus::Inspection {
            (split[0], None, None, Some(split[1]))
        } else {
            (split[0], Some(split[1]), None, None)
        }
    };

    // Overview panel
    hit_map.add(overview_area, HitTarget::OverviewPanel);

    // Overview table rows
    if !app.visible.is_empty() && overview_area.height >= 4 {
        let available_rows = overview_area.height.saturating_sub(3) as usize;
        let offset = calculate_table_offset(app.selected, app.visible.len(), available_rows);
        let rows_to_display = (app.visible.len().saturating_sub(offset)).min(available_rows);

        for row_idx in 0..rows_to_display {
            let vis_index = offset + row_idx;
            let row_y = overview_area.y + 2 + row_idx as u16;
            let row_rect = Rect {
                x: overview_area.x + 1,
                y: row_y,
                width: overview_area.width.saturating_sub(2),
                height: 1,
            };
            hit_map.add(row_rect, HitTarget::OverviewRow(vis_index));
        }
    }

    if let Some(details) = details_area {
        hit_map.add(details, HitTarget::DetailsPanel);
    }
    if let Some(connections) = connections_area {
        hit_map.add(connections, HitTarget::ConnectionsPanel);
    }
    if let Some(inspection) = inspection_area {
        hit_map.add(inspection, HitTarget::InspectionPanel);
    }

    // Footer clickable actions
    let footer_area = root[2];
    if footer_area.height >= 2 {
        let footer = footer_layout(area, app);
        let bounds = Rect::new(
            footer_area.x,
            footer_area.y.saturating_add(1),
            footer_area.width,
            1,
        );
        for control in footer.controls {
            add_if_fully_visible(
                &mut hit_map,
                bounds,
                Rect::new(control.x, bounds.y, control.width, 1),
                control.target,
            );
        }
    }

    hit_map
}

pub fn handle_mouse_event(
    app: &mut App,
    hover: &mut HoverState,
    hit_map: &HitMap,
    mouse: MouseEvent,
) -> Result<()> {
    match mouse.kind {
        MouseEventKind::Moved => {
            hover.target = hit_map.hit_test(mouse.column, mouse.row);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            hover.target = hit_map.hit_test(mouse.column, mouse.row);
            let Some(target) = hover.target else {
                return Ok(());
            };

            match target {
                HitTarget::OverviewRow(index) => {
                    app.focus = Focus::Overview;
                    app.select_visible_index(index);
                }
                HitTarget::OverviewPanel => {
                    app.focus = Focus::Overview;
                }
                HitTarget::DetailsPanel => {
                    app.focus = Focus::Overview;
                }
                HitTarget::ConnectionsPanel => {
                    app.focus = Focus::Connections;
                }
                HitTarget::InspectionPanel => {
                    app.focus = Focus::Inspection;
                }
                HitTarget::FooterSearch => {
                    app.begin_search();
                }
                HitTarget::FooterFocus => {
                    app.focus = app.focus.next();
                }
                HitTarget::FooterPath => {
                    app.show_binary_path();
                }
                HitTarget::FooterKill => {
                    app.request_confirmation(ConfirmKind::Terminate);
                }
                HitTarget::FooterHelp => {
                    app.overlay = Overlay::Help;
                }
                HitTarget::FooterQuit => {
                    app.should_quit = true;
                }
                HitTarget::SearchApply => {
                    app.search_query = app.search_input.trim().to_owned();
                    app.overlay = Overlay::None;
                    app.recompute_visible();
                    app.status = if app.search_query.is_empty() {
                        Some("search cleared".to_owned())
                    } else {
                        Some(format!("searching for {}", app.search_query))
                    };
                }
                HitTarget::SearchCancel => {
                    app.search_input.clear();
                    app.overlay = Overlay::None;
                }
                HitTarget::HelpClose | HitTarget::BinaryPathClose => {
                    app.overlay = Overlay::None;
                }
                HitTarget::ConfirmExecute => {
                    if let Overlay::Confirm(confirmation) = &app.overlay {
                        if confirmation.is_blocked() {
                            if let Some(reason) = &confirmation.blocked_reason {
                                let reason = reason.clone();
                                app.overlay = Overlay::None;
                                app.error = Some(reason);
                            }
                        } else {
                            match confirmation.kind {
                                ConfirmKind::Terminate => {
                                    let _ = app.confirm();
                                }
                                ConfirmKind::Kill => {
                                    if confirmation.input.trim() == "KILL" {
                                        let _ = app.confirm();
                                    } else {
                                        app.status =
                                            Some("type KILL to confirm force-kill".to_owned());
                                    }
                                }
                            }
                        }
                    }
                }
                HitTarget::ConfirmCancel => {
                    app.overlay = Overlay::None;
                    app.status = Some("action cancelled".to_owned());
                }
                HitTarget::ConfirmDismiss => {
                    if let Overlay::Confirm(confirmation) = &app.overlay {
                        if let Some(reason) = &confirmation.blocked_reason {
                            let reason = reason.clone();
                            app.overlay = Overlay::None;
                            app.error = Some(reason);
                        }
                    }
                }
                HitTarget::ModalBackdrop => {
                    // Modal backdrop safely traps clicks and no-ops.
                }
            }
        }
        MouseEventKind::ScrollDown => {
            hover.target = hit_map.hit_test(mouse.column, mouse.row);
            if let Some(HitTarget::OverviewRow(_) | HitTarget::OverviewPanel) = hover.target {
                app.move_selection_by(1);
            }
        }
        MouseEventKind::ScrollUp => {
            hover.target = hit_map.hit_test(mouse.column, mouse.row);
            if let Some(HitTarget::OverviewRow(_) | HitTarget::OverviewPanel) = hover.target {
                app.move_selection_by(-1);
            }
        }
        _ => {
            // Drag, double-click, and other buttons intentionally no-op.
        }
    }
    Ok(())
}

pub fn handle_event(
    app: &mut App,
    hover: &mut HoverState,
    hit_map: &mut HitMap,
    event: Event,
) -> Result<()> {
    match event {
        Event::Key(key) => {
            app.handle_key(key)?;
        }
        Event::Mouse(mouse) => {
            handle_mouse_event(app, hover, hit_map, mouse)?;
        }
        Event::Resize(width, height) => {
            *hit_map = build_hit_map(Rect::new(0, 0, width, height), app);
            hover.target = None;
        }
        Event::FocusGained | Event::FocusLost | Event::Paste(_) => {
            // Focus changes and bracketed paste are intentionally ignored;
            // only explicit key, mouse, and resize events affect state.
        }
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();
    let mut hover = HoverState::default();
    let mut hit_map = HitMap::new();
    terminal.clear()?;

    loop {
        terminal.draw(|frame| {
            hit_map = build_hit_map(frame.area(), &app);
            render_with_hover(frame, &app, &hover);
        })?;
        if event::poll(EVENT_POLL)? {
            let event = event::read()?;
            handle_event(&mut app, &mut hover, &mut hit_map, event)?;
        }
        app.tick();
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
pub fn render(frame: &mut Frame<'_>, app: &App) {
    render_with_hover(frame, app, &HoverState::default());
}

pub fn render_with_hover(frame: &mut Frame<'_>, app: &App, hover: &HoverState) {
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
        render_wide(frame, root[1], app, hover, theme);
    } else {
        render_narrow(frame, root[1], app, hover, theme);
    }
    render_footer(frame, root[2], app, hover, theme);
    match &app.overlay {
        Overlay::Help => help::render(frame, area, theme),
        Overlay::Search => render_search_overlay(frame, area, app, hover, theme),
        Overlay::BinaryPath(path) => render_binary_path(frame, area, path, hover, theme),
        Overlay::Confirm(confirmation) => {
            render_confirmation(frame, area, confirmation, hover, theme)
        }
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

fn render_wide(frame: &mut Frame<'_>, area: Rect, app: &App, hover: &HoverState, theme: Theme) {
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(59), Constraint::Percentage(41)])
        .split(area);
    render_overview(frame, split[0], app, hover, theme);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(5)])
        .split(split[1]);
    if app.focus == Focus::Inspection {
        render_inspection(frame, split[1], app, hover, theme);
    } else {
        render_details(
            frame,
            right[0],
            app.selected_service(),
            app.focus == Focus::Connections,
            hover,
            theme,
        );
        render_connections(
            frame,
            right[1],
            app.selected_service(),
            app.focus == Focus::Connections,
            hover,
            theme,
        );
    }
}

fn render_narrow(frame: &mut Frame<'_>, area: Rect, app: &App, hover: &HoverState, theme: Theme) {
    if area.height < 15 {
        render_overview(frame, area, app, hover, theme);
        return;
    }
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    render_overview(frame, split[0], app, hover, theme);
    if app.focus == Focus::Inspection {
        render_inspection(frame, split[1], app, hover, theme);
    } else {
        render_details(
            frame,
            split[1],
            app.selected_service(),
            app.focus == Focus::Connections,
            hover,
            theme,
        );
    }
}

fn render_overview(frame: &mut Frame<'_>, area: Rect, app: &App, hover: &HoverState, theme: Theme) {
    let title = if app.search_query.is_empty() {
        "Services".to_owned()
    } else {
        format!("Services · /{}", app.search_query)
    };
    let border_style = if app.focus == Focus::Overview {
        Style::default().fg(theme.accent)
    } else if hover.target == Some(HitTarget::OverviewPanel) {
        theme.hover_border()
    } else {
        theme.border()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style)
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

    let rows = app.visible.iter().enumerate().map(|(vis_idx, index)| {
        let service = &app.services[*index];
        let process = if service.process.name.is_empty() {
            format!("PID {}", service.process.pid)
        } else {
            format!("{}  {}", service.process.name, service.process.pid)
        };
        let is_hovered = hover.target == Some(HitTarget::OverviewRow(vis_idx));
        let is_selected = vis_idx == app.selected;

        let row_style = if is_selected {
            theme.selected()
        } else if is_hovered {
            theme.hover_row()
        } else if service.state.is_listening() {
            theme.good()
        } else {
            Style::default().fg(theme.text)
        };

        Row::new(vec![
            Cell::from(service.protocol.as_str()),
            Cell::from(service.local.to_string()),
            Cell::from(scope_badge(service.scope)).style(theme.exposure(service.scope)),
            Cell::from(process),
            Cell::from(service.state.to_string()),
            Cell::from(service.connections.len().to_string()),
        ])
        .style(row_style)
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
    hover: &HoverState,
    theme: Theme,
) {
    let border_style = if !connections_focus {
        Style::default().fg(theme.accent)
    } else if hover.target == Some(HitTarget::DetailsPanel) {
        theme.hover_border()
    } else {
        theme.border()
    };
    let block = Block::default()
        .title("Selected service")
        .borders(Borders::ALL)
        .border_style(border_style)
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
    hover: &HoverState,
    theme: Theme,
) {
    let border_style = if focused {
        Style::default().fg(theme.accent)
    } else if hover.target == Some(HitTarget::ConnectionsPanel) {
        theme.hover_border()
    } else {
        theme.border()
    };
    let block = Block::default()
        .title("Connections")
        .borders(Borders::ALL)
        .border_style(border_style)
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

fn render_inspection(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    hover: &HoverState,
    theme: Theme,
) {
    let border_style = if app.focus == Focus::Inspection {
        Style::default().fg(theme.accent)
    } else if hover.target == Some(HitTarget::InspectionPanel) {
        theme.hover_border()
    } else {
        theme.border()
    };
    let block = Block::default()
        .title("Inspection history · newest first")
        .borders(Borders::ALL)
        .border_style(border_style)
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

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, hover: &HoverState, theme: Theme) {
    let footer = footer_layout(area, app);
    let left_style = if app.error.is_some() {
        theme.danger()
    } else if app.status.is_some() {
        theme.muted()
    } else {
        theme.muted()
    };
    let mut spans = vec![
        Span::styled(footer.left, left_style),
        Span::raw("  "),
        Span::styled(format!("last refresh {}", footer.refreshed), theme.muted()),
        Span::raw("                                      "),
        Span::styled("↑↓/jk", Style::default().fg(theme.accent)),
        Span::raw(" move  "),
    ];

    for (index, control) in footer.controls.iter().enumerate() {
        let hovered = hover.target == Some(control.target);
        let key_style = match control.target {
            HitTarget::FooterKill if hovered => theme.hover_danger_button(),
            _ if hovered => theme.hover_button(),
            HitTarget::FooterKill => Style::default().fg(theme.accent),
            _ => Style::default().fg(theme.accent),
        };
        let label_style = if hovered {
            key_style
        } else {
            Style::default().fg(theme.text)
        };
        spans.push(Span::styled(control.key, key_style));
        spans.push(Span::styled(control.label, label_style));
        if index + 1 < footer.controls.len() {
            spans.push(Span::raw("  "));
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(theme.border()),
            )
            .style(theme.panel()),
        area,
    );
}

fn render_search_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    hover: &HoverState,
    theme: Theme,
) {
    let modal = search_modal(area);
    frame.render_widget(Clear, modal);

    let apply_style = if hover.target == Some(HitTarget::SearchApply) {
        theme.hover_button()
    } else {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    };
    let cancel_style = if hover.target == Some(HitTarget::SearchCancel) {
        theme.hover_button()
    } else {
        theme.muted()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "/ ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&app.search_input),
            Span::styled("▌", Style::default().fg(theme.accent)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Enter", apply_style),
            Span::styled(" applies", apply_style),
            Span::raw("    "),
            Span::styled("Esc", cancel_style),
            Span::styled(" cancels", cancel_style),
        ]),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title("Search services")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.accent))
                    .style(theme.panel()),
            )
            .style(Style::default().fg(theme.text)),
        modal,
    );
}
fn confirmation_body_lines(
    confirmation: &crate::app::Confirmation,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let user = confirmation.process.user.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("Target:  ".to_owned(), theme.muted()),
        Span::styled(
            format!(
                "{} (PID {})",
                confirmation.process.name, confirmation.process.pid
            ),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("User: ".to_owned(), theme.muted()),
        Span::styled(
            user.to_owned(),
            if user == "root" {
                theme.warning()
            } else {
                Style::default().fg(theme.text)
            },
        ),
    ]));

    if let Some(command) = &confirmation.process.command {
        lines.push(Line::from(vec![
            Span::styled("Command: ".to_owned(), theme.muted()),
            Span::styled(command.clone(), theme.muted()),
        ]));
    } else if let Some(executable) = &confirmation.process.executable {
        lines.push(Line::from(vec![
            Span::styled("Binary:  ".to_owned(), theme.muted()),
            Span::styled(executable.display().to_string(), theme.muted()),
        ]));
    }

    let sockets_str = if confirmation.sockets.is_empty() {
        confirmation.endpoint.clone()
    } else {
        confirmation.sockets.join(", ")
    };
    lines.push(Line::from(vec![
        Span::styled("Sockets: ".to_owned(), theme.muted()),
        Span::styled(sockets_str, Style::default().fg(theme.accent)),
    ]));

    if let Some(reason) = &confirmation.blocked_reason {
        lines.push(Line::from(vec![
            Span::styled(
                "Blocked: ".to_owned(),
                theme.danger().add_modifier(Modifier::BOLD),
            ),
            Span::styled(reason.clone(), theme.danger()),
        ]));
    } else if confirmation.connection_count > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                "Warning: ".to_owned(),
                theme.warning().add_modifier(Modifier::BOLD),
            ),
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
            Span::styled(
                "Note: ".to_owned(),
                theme.warning().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Process is owned by root and may require elevated privileges.".to_owned(),
                theme.warning(),
            ),
        ]));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(""));
    lines
}

fn render_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    confirmation: &crate::app::Confirmation,
    hover: &HoverState,
    theme: Theme,
) {
    let layout = confirmation_action_layout(area, confirmation);
    let modal = layout.modal;
    frame.render_widget(Clear, modal);

    let (title, border_style) = if confirmation.is_blocked() {
        ("Blocked action · Esc to dismiss", theme.danger())
    } else {
        match confirmation.kind {
            crate::app::ConfirmKind::Terminate => ("Terminate process (SIGTERM)", theme.warning()),
            crate::app::ConfirmKind::Kill => ("Force-kill process (SIGKILL)", theme.danger()),
        }
    };

    let mut lines = confirmation_body_lines(confirmation, theme);

    if layout.kill_input_row {
        lines.push(Line::from(vec![
            Span::styled("Type KILL to confirm: ".to_owned(), theme.muted()),
            Span::styled(
                format!("[ {}▌ ]", confirmation.input),
                theme.danger().add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    for row in &layout.rows {
        let mut spans = Vec::new();
        for (index, control) in row.iter().enumerate() {
            let style = if confirmation.is_blocked() {
                if hover.target == Some(control.target) {
                    theme.hover_button()
                } else {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                match (confirmation.kind, control.target) {
                    (ConfirmKind::Terminate, HitTarget::ConfirmExecute) => {
                        if hover.target == Some(control.target) {
                            theme.hover_danger_button()
                        } else {
                            theme.danger().add_modifier(Modifier::BOLD)
                        }
                    }
                    (ConfirmKind::Kill, HitTarget::ConfirmExecute) => {
                        if hover.target == Some(control.target) {
                            if confirmation.input.trim() == "KILL" {
                                theme.hover_danger_button()
                            } else {
                                theme.hover_button()
                            }
                        } else if confirmation.input.trim() == "KILL" {
                            theme.danger().add_modifier(Modifier::BOLD)
                        } else {
                            theme.muted()
                        }
                    }
                    (_, HitTarget::ConfirmCancel) => {
                        if hover.target == Some(control.target) {
                            theme.hover_button()
                        } else {
                            Style::default().fg(theme.accent)
                        }
                    }
                    _ => theme.muted(),
                }
            };
            spans.push(Span::styled(control.key, style));
            spans.push(Span::styled(control.label, style));
            if index + 1 < row.len() {
                spans.push(Span::raw("    "));
            }
        }
        lines.push(Line::from(spans));
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

fn render_binary_path(
    frame: &mut Frame<'_>,
    area: Rect,
    path: &str,
    hover: &HoverState,
    theme: Theme,
) {
    let modal = binary_path_modal(area);
    frame.render_widget(Clear, modal);

    let close_style = if hover.target == Some(HitTarget::BinaryPathClose) {
        theme.hover_button()
    } else {
        Style::default().fg(theme.accent)
    };

    let title = Line::from(vec![
        Span::raw("Binary path · "),
        Span::styled("Esc closes", close_style),
    ]);

    frame.render_widget(
        Paragraph::new(path)
            .block(
                Block::default()
                    .title(title)
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

    fn make_test_service(pid: u32, name: &str, port: u16) -> ServiceRecord {
        ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port),
            SocketState::Listening,
            ProcessMetadata::new(pid, name),
            None,
        )
    }

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
                    &HoverState::default(),
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

    #[test]
    fn hit_map_calculates_geometry_wide_and_narrow() {
        let app = App::from_services(vec![
            make_test_service(100, "node", 3000),
            make_test_service(200, "redis", 6379),
        ]);

        // Wide terminal
        let wide_area = Rect::new(0, 0, 120, 30);
        let wide_map = build_hit_map(wide_area, &app);

        // Header is at y=0..3
        // Body is at y=3..28
        // Overview rows start at y = 3 + 2 = 5
        assert_eq!(wide_map.hit_test(10, 5), Some(HitTarget::OverviewRow(0)));
        assert_eq!(wide_map.hit_test(10, 6), Some(HitTarget::OverviewRow(1)));

        // Footer controls at y = 29; the rendered search span defines its own x.
        let search_rect = wide_map
            .entries
            .iter()
            .find(|(_, target)| *target == HitTarget::FooterSearch)
            .map(|(rect, _)| *rect)
            .expect("search is visible at width 120");
        assert_eq!(
            wide_map.hit_test(search_rect.x, search_rect.y),
            Some(HitTarget::FooterSearch)
        );
        assert_ne!(
            wide_map.hit_test(search_rect.x.saturating_sub(1), search_rect.y),
            Some(HitTarget::FooterSearch)
        );
        let narrow_area = Rect::new(0, 0, 80, 24);
        let narrow_map = build_hit_map(narrow_area, &app);
        assert_eq!(narrow_map.hit_test(10, 5), Some(HitTarget::OverviewRow(0)));
    }

    #[test]
    fn footer_hitboxes_follow_rendered_controls_and_omit_clipped_controls() {
        let app = App::from_services(vec![make_test_service(100, "node", 3000)]);
        for (width, height) in [(120, 30), (80, 24)] {
            let area = Rect::new(0, 0, width, height);
            let hit_map = build_hit_map(area, &app);
            let footer = footer_layout(area, &app);
            let footer_y = height - 1;
            for control in footer.controls {
                let visible = control.x.saturating_add(control.width) <= width;
                let registered = hit_map.entries.iter().any(|(rect, target)| {
                    *target == control.target
                        && *rect == Rect::new(control.x, footer_y, control.width, 1)
                });
                assert_eq!(
                    registered, visible,
                    "{:?} visibility mismatch at width {width}",
                    control.target
                );
                if visible {
                    assert_eq!(
                        hit_map.hit_test(control.x, footer_y),
                        Some(control.target),
                        "{:?} does not map to its rendered span",
                        control.target
                    );
                }
            }
        }
    }

    #[test]
    fn modal_hitboxes_match_visible_labels_and_leave_blank_text_inert() {
        let area = Rect::new(0, 0, 120, 30);

        let mut search = App::default();
        search.overlay = Overlay::Search;
        let search_map = build_hit_map(area, &search);
        let (_, apply, cancel) = search_controls(area);
        assert_eq!(
            search_map.hit_test(apply.x, apply.y),
            Some(HitTarget::SearchApply)
        );
        assert_eq!(
            search_map.hit_test(cancel.x, cancel.y),
            Some(HitTarget::SearchCancel)
        );
        assert_eq!(
            search_map.hit_test(apply.x + apply.width, apply.y),
            Some(HitTarget::ModalBackdrop)
        );

        let mut help_app = App::default();
        help_app.overlay = Overlay::Help;
        let help_map = build_hit_map(area, &help_app);
        let help_modal = help::modal_rect(area);
        let close_width = text_width(help::CLOSE_LABEL);
        let close_x = help_modal.x + help_modal.width - 1 - close_width;
        assert_eq!(
            help_map.hit_test(close_x, help_modal.y),
            Some(HitTarget::HelpClose)
        );

        let mut path_service = make_test_service(100, "node", 3000);
        path_service.process.executable = Some(PathBuf::from("/usr/bin/node"));
        let mut path_app = App::from_services(vec![path_service]);
        path_app.show_binary_path();
        let path_map = build_hit_map(area, &path_app);
        let (_, path_close) = binary_path_close_rect(area);
        for x in path_close.x..path_close.x.saturating_add(path_close.width) {
            assert_eq!(
                path_map.hit_test(x, path_close.y),
                Some(HitTarget::BinaryPathClose),
                "binary path close label cell {x} is not clickable",
            );
        }
        assert_eq!(
            path_map.hit_test(path_close.x.saturating_sub(1), path_close.y),
            Some(HitTarget::ModalBackdrop),
        );
        assert_eq!(
            path_map.hit_test(path_close.x.saturating_add(path_close.width), path_close.y,),
            Some(HitTarget::ModalBackdrop),
        );

        let mut confirm_app = App::from_services(vec![make_test_service(100, "node", 3000)]);
        confirm_app.request_confirmation(ConfirmKind::Terminate);
        let confirm_map = build_hit_map(area, &confirm_app);
        let confirm_layout = confirmation_action_layout(
            area,
            match &confirm_app.overlay {
                Overlay::Confirm(confirmation) => confirmation,
                _ => unreachable!(),
            },
        );
        for (rect, target) in &confirm_layout.controls {
            assert_eq!(confirm_map.hit_test(rect.x, rect.y), Some(*target));
        }
        let exec = confirm_layout
            .controls
            .iter()
            .find(|(_, target)| *target == HitTarget::ConfirmExecute)
            .map(|(rect, _)| *rect)
            .expect("terminate action is visible");
        assert_eq!(
            confirm_map.hit_test(exec.x + exec.width, exec.y),
            Some(HitTarget::ModalBackdrop)
        );
    }

    #[test]
    fn focus_and_paste_events_are_ignored() {
        let mut app = App::from_services(vec![make_test_service(100, "node", 3000)]);
        app.focus = Focus::Connections;
        let area = Rect::new(0, 0, 120, 30);
        let mut hit_map = build_hit_map(area, &app);
        let mut hover = HoverState {
            target: Some(HitTarget::OverviewRow(0)),
        };

        let initial_selected = app.selected;
        let initial_focus = app.focus;
        let initial_overlay = app.overlay.clone();
        let initial_status = app.status.clone();
        let initial_error = app.error.clone();
        let initial_should_quit = app.should_quit;
        let initial_hover = hover;
        let initial_hit = hit_map.hit_test(10, 5);

        for event in [
            Event::FocusGained,
            Event::FocusLost,
            Event::Paste("q".to_owned()),
        ] {
            handle_event(&mut app, &mut hover, &mut hit_map, event).unwrap();
        }

        assert_eq!(app.selected, initial_selected);
        assert_eq!(app.focus, initial_focus);
        assert_eq!(app.overlay, initial_overlay);
        assert_eq!(app.status, initial_status);
        assert_eq!(app.error, initial_error);
        assert_eq!(app.should_quit, initial_should_quit);
        assert_eq!(hover, initial_hover);
        assert_eq!(hit_map.hit_test(10, 5), initial_hit);
    }

    #[test]
    fn mouse_left_click_selects_visible_row_and_focuses_overview() {
        let mut app = App::from_services(vec![
            make_test_service(100, "first", 3000),
            make_test_service(200, "second", 3001),
            make_test_service(300, "third", 3002),
        ]);
        app.focus = Focus::Connections;
        app.selected = 0;

        let area = Rect::new(0, 0, 120, 30);
        let hit_map = build_hit_map(area, &app);
        let mut hover = HoverState::default();

        // Click row 2 (y = 3 + 2 + 2 = 7)
        let click_event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 7,
            modifiers: event::KeyModifiers::empty(),
        };

        handle_mouse_event(&mut app, &mut hover, &hit_map, click_event).unwrap();
        assert_eq!(app.selected, 2);
        assert_eq!(app.focus, Focus::Overview);
    }

    #[test]
    fn mouse_wheel_moves_selection_and_clamps() {
        let mut app = App::from_services(vec![
            make_test_service(100, "first", 3000),
            make_test_service(200, "second", 3001),
        ]);
        app.selected = 0;

        let area = Rect::new(0, 0, 120, 30);
        let hit_map = build_hit_map(area, &app);
        let mut hover = HoverState::default();

        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 5,
            modifiers: event::KeyModifiers::empty(),
        };
        handle_mouse_event(&mut app, &mut hover, &hit_map, scroll_down).unwrap();
        assert_eq!(app.selected, 1);

        // Scroll down again: clamped to max index 1
        handle_mouse_event(&mut app, &mut hover, &hit_map, scroll_down).unwrap();
        assert_eq!(app.selected, 1);

        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 5,
            modifiers: event::KeyModifiers::empty(),
        };
        handle_mouse_event(&mut app, &mut hover, &hit_map, scroll_up).unwrap();
        assert_eq!(app.selected, 0);

        // Scroll up again: clamped to 0
        handle_mouse_event(&mut app, &mut hover, &hit_map, scroll_up).unwrap();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn overlay_traps_all_events_and_underlying_elements_are_unreachable() {
        let mut app = App::from_services(vec![
            make_test_service(100, "first", 3000),
            make_test_service(200, "second", 3001),
        ]);
        app.overlay = Overlay::Help;

        let area = Rect::new(0, 0, 120, 30);
        let hit_map = build_hit_map(area, &app);

        // Click where overview row was at (10, 5) -> now modal backdrop, not OverviewRow
        assert_eq!(hit_map.hit_test(10, 5), Some(HitTarget::ModalBackdrop));

        let mut hover = HoverState::default();
        let click_event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: event::KeyModifiers::empty(),
        };

        let prev_selected = app.selected;
        handle_mouse_event(&mut app, &mut hover, &hit_map, click_event).unwrap();
        // Selection must not change
        assert_eq!(app.selected, prev_selected);
        // Overlay is still Help
        assert_eq!(app.overlay, Overlay::Help);
    }

    #[test]
    fn footer_controls_trigger_expected_app_actions() {
        let mut app = App::from_services(vec![make_test_service(100, "node", 3000)]);
        let area = Rect::new(0, 0, 120, 30);
        let hit_map = build_hit_map(area, &app);
        let mut hover = HoverState::default();

        // Find footer search target
        let search_target = HitTarget::FooterSearch;
        let search_pos = hit_map
            .entries
            .iter()
            .find(|(_, t)| *t == search_target)
            .map(|(r, _)| (r.x, r.y))
            .unwrap();

        let click_search = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: search_pos.0,
            row: search_pos.1,
            modifiers: event::KeyModifiers::empty(),
        };
        handle_mouse_event(&mut app, &mut hover, &hit_map, click_search).unwrap();
        assert_eq!(app.overlay, Overlay::Search);
    }

    #[test]
    fn confirm_modal_retains_destructive_kill_gate() {
        let service = make_test_service(5432, "postgres", 5432);
        let mut app = App::from_services(vec![service]);
        app.request_confirmation(ConfirmKind::Kill);

        let area = Rect::new(0, 0, 120, 30);
        let hit_map = build_hit_map(area, &app);
        let mut hover = HoverState::default();

        let exec_pos = hit_map
            .entries
            .iter()
            .find(|(_, t)| *t == HitTarget::ConfirmExecute)
            .map(|(r, _)| (r.x, r.y))
            .unwrap();

        let click_exec = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: exec_pos.0,
            row: exec_pos.1,
            modifiers: event::KeyModifiers::empty(),
        };

        // Click when input is NOT "KILL" -> must NOT execute confirm, sets status message
        handle_mouse_event(&mut app, &mut hover, &hit_map, click_exec).unwrap();
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        assert_eq!(
            app.status.as_deref(),
            Some("type KILL to confirm force-kill")
        );

        // Type KILL
        if let Overlay::Confirm(c) = &mut app.overlay {
            c.input = "KILL".to_owned();
        }

        // Click again with input "KILL"
        handle_mouse_event(&mut app, &mut hover, &hit_map, click_exec).unwrap();
        // Overlay is dismissed
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn hover_styling_changes_visual_presentation_without_mutating_app() {
        let app = App::from_services(vec![
            make_test_service(100, "node", 3000),
            make_test_service(200, "redis", 6379),
        ]);
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let hover = HoverState {
            target: Some(HitTarget::OverviewRow(1)),
        };

        terminal
            .draw(|frame| render_with_hover(frame, &app, &hover))
            .unwrap();

        // App state was not modified
        assert_eq!(app.selected, 0);
        assert_eq!(app.focus, Focus::Overview);
        assert_eq!(app.overlay, Overlay::None);
    }
}
