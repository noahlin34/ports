use std::{
    collections::{HashSet, VecDeque},
    fmt,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ports::{
    discovery::{discover, terminate_pid},
    filter::Filter,
    model::{ProcessMetadata, Protocol, ServiceRecord},
};

const HISTORY_LIMIT: usize = 128;
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(900);

/// Stable identity for a grouped service row. Binding addresses are
/// intentionally absent so refreshes and dual-stack collapse do not move the
/// selection.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServiceKey {
    pub protocol: Protocol,
    pub port: u16,
    pub pid: u32,
}

impl ServiceKey {
    pub fn from_service(service: &ServiceRecord) -> Self {
        Self {
            protocol: service.protocol,
            port: service.local.port,
            pid: service.process.pid,
        }
    }
}

/// The primary data set shown in the table. This is intentionally separate
/// from [`Focus`], which controls the secondary detail panels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ViewMode {
    #[default]
    Services,
    Connections,
    All,
}

impl ViewMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Connections => "Connections",
            Self::All => "All",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Services => Self::Connections,
            Self::Connections => Self::All,
            Self::All => Self::Services,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Services => Self::All,
            Self::Connections => Self::Services,
            Self::All => Self::Connections,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConnectionKey {
    protocol: Protocol,
    local_port: u16,
    remote: ports::model::RemoteEndpoint,
    pid: u32,
}

impl ConnectionKey {
    fn from_connection(connection: &ports::model::ConnectionRecord) -> Self {
        Self {
            protocol: connection.protocol,
            local_port: connection.local.port,
            remote: connection.remote.clone(),
            pid: connection.process.pid,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RowKey {
    Service(ServiceKey),
    Connection(ConnectionKey),
}

/// A row in the primary table. Connections retain their owning service so
/// details and process actions continue to work for listener-attached peers.
#[derive(Clone, Copy, Debug)]
pub enum ViewRow<'a> {
    Service(&'a ServiceRecord),
    Connection {
        service: &'a ServiceRecord,
        connection: &'a ports::model::ConnectionRecord,
    },
}

impl<'a> ViewRow<'a> {
    pub fn service(self) -> &'a ServiceRecord {
        match self {
            Self::Service(service) | Self::Connection { service, .. } => service,
        }
    }

    pub fn connection(self) -> Option<&'a ports::model::ConnectionRecord> {
        match self {
            Self::Service(_) => None,
            Self::Connection { connection, .. } => Some(connection),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Overview,
    Connections,
    Inspection,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Self::Overview => Self::Connections,
            Self::Connections => Self::Inspection,
            Self::Inspection => Self::Overview,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmKind {
    Terminate,
    Kill,
}

impl ConfirmKind {
    pub const fn signal(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    pub const fn force(self) -> bool {
        matches!(self, Self::Kill)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Confirmation {
    pub kind: ConfirmKind,
    pub process: ProcessMetadata,
    pub endpoint: String,
    pub sockets: Vec<String>,
    pub connection_count: usize,
    pub blocked_reason: Option<String>,
    pub input: String,
}

impl Confirmation {
    pub fn is_blocked(&self) -> bool {
        self.blocked_reason.is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryKind {
    Opened,
    Closed,
    ProcessExited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEvent {
    pub at: Instant,
    pub kind: HistoryKind,
    pub detail: String,
}

impl HistoryEvent {
    fn new(kind: HistoryKind, detail: String) -> Self {
        Self {
            at: Instant::now(),
            kind,
            detail,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self.kind {
            HistoryKind::Opened => "+",
            HistoryKind::Closed => "−",
            HistoryKind::ProcessExited => "×",
        }
    }
}

impl fmt::Display for HistoryEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.symbol(), self.detail)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Overlay {
    #[default]
    None,
    Search,
    Help,
    BinaryPath(String),
    Confirm(Box<Confirmation>),
}

/// All mutable UI state lives here. Rendering is intentionally a pure view of
/// this value; refreshes can therefore replace discovery records without
/// invalidating a selected socket or leaving stale modal state behind.
pub struct App {
    pub services: Vec<ServiceRecord>,
    /// Legacy service indexes retained for consumers that only render service
    /// rows. New renderers should use [`Self::visible_rows`].
    pub visible: Vec<usize>,
    view_rows: Vec<RowIndex>,
    pub selected: usize,
    pub selected_key: Option<ServiceKey>,
    selected_row_key: Option<RowKey>,
    pub view_mode: ViewMode,
    pub focus: Focus,
    pub overlay: Overlay,
    pub search_query: String,
    pub search_input: String,
    pub history: VecDeque<HistoryEvent>,
    pub last_refresh: Option<Instant>,
    pub next_refresh: Instant,
    pub refresh_count: u64,
    pub error: Option<String>,
    pub status: Option<String>,
    pub should_quit: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowIndex {
    Service(usize),
    Connection {
        service_index: usize,
        connection_index: usize,
    },
}

impl Default for App {
    fn default() -> Self {
        Self {
            services: Vec::new(),
            visible: Vec::new(),
            view_rows: Vec::new(),
            selected: 0,
            selected_key: None,
            selected_row_key: None,
            view_mode: ViewMode::Services,
            focus: Focus::Overview,
            overlay: Overlay::None,
            search_query: String::new(),
            search_input: String::new(),
            history: VecDeque::with_capacity(HISTORY_LIMIT),
            last_refresh: None,
            next_refresh: Instant::now(),
            refresh_count: 0,
            error: None,
            status: None,
            should_quit: false,
        }
    }
}

fn sort_services(services: &mut [ServiceRecord]) {
    services.sort_by(|left, right| {
        right
            .state
            .is_listening()
            .cmp(&left.state.is_listening())
            .then_with(|| left.local.port.cmp(&right.local.port))
            .then_with(|| left.protocol.cmp(&right.protocol))
            .then_with(|| left.local.cmp(&right.local))
            .then_with(|| left.process.pid.cmp(&right.process.pid))
            .then_with(|| left.process.name.cmp(&right.process.name))
    });
}

impl App {
    /// Create the live application. Discovery failures are retained as an
    /// informative screen state so permission errors do not crash the TUI.
    pub fn new() -> Self {
        let mut app = Self::default();
        let _ = app.refresh();
        app
    }

    #[cfg(test)]
    pub fn from_services(mut services: Vec<ServiceRecord>) -> Self {
        sort_services(&mut services);
        let mut app = Self::default();
        app.services = services;
        app.recompute_visible();
        app.last_refresh = Some(Instant::now());
        app.next_refresh = Instant::now() + REFRESH_INTERVAL;
        app
    }

    pub fn refresh_due(&self, now: Instant) -> bool {
        now >= self.next_refresh && matches!(self.overlay, Overlay::None)
    }

    pub fn tick(&mut self) {
        if self.refresh_due(Instant::now()) {
            let _ = self.refresh();
        }
    }

    pub fn refresh(&mut self) -> Result<()> {
        match discover() {
            Ok(services) => {
                if self.last_refresh.is_none() {
                    let mut services = services;
                    sort_services(&mut services);
                    self.services = services;
                    self.recompute_visible();
                } else {
                    self.replace_services(services);
                }
                self.last_refresh = Some(Instant::now());
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                self.refresh_count = self.refresh_count.saturating_add(1);
                self.error = None;
                self.status = Some(format!("refreshed · {} services", self.services.len()));
                Ok(())
            }
            Err(error) => {
                let message = format_discovery_error(&error);
                self.error = Some(message);
                self.status = Some("refresh failed · retrying automatically".to_owned());
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                Err(error)
            }
        }
    }

    /// Replace records from a discovery pass and record the meaningful diff.
    /// The selected key is resolved after sorting and filtering, preserving a
    /// row even when lsof changes the order between refreshes.
    pub fn replace_services(&mut self, mut services: Vec<ServiceRecord>) {
        sort_services(&mut services);

        let previous = self
            .services
            .iter()
            .map(ServiceKey::from_service)
            .collect::<HashSet<_>>();
        let next = services
            .iter()
            .map(ServiceKey::from_service)
            .collect::<HashSet<_>>();
        let previous_pids = self
            .services
            .iter()
            .map(|service| service.process.pid)
            .collect::<HashSet<_>>();
        let next_pids = services
            .iter()
            .map(|service| service.process.pid)
            .collect::<HashSet<_>>();

        let mut events = Vec::new();
        for service in &services {
            let key = ServiceKey::from_service(service);
            if !previous.contains(&key) {
                events.push((HistoryKind::Opened, service_label(service)));
            }
        }
        for service in &self.services {
            let key = ServiceKey::from_service(service);
            if !next.contains(&key) {
                events.push((HistoryKind::Closed, service_label(service)));
            }
        }
        for service in &self.services {
            let pid = service.process.pid;
            if !next_pids.contains(&pid) && previous_pids.contains(&pid) {
                events.push((
                    HistoryKind::ProcessExited,
                    format!("{} (PID {})", service.process.name, pid),
                ));
            }
        }
        for (kind, detail) in events {
            self.push_history(kind, detail);
        }

        self.services = services;
        self.recompute_visible();
    }

    pub fn selected_service(&self) -> Option<&ServiceRecord> {
        self.selected_row().map(ViewRow::service)
    }

    pub fn selected_service_key(&self) -> Option<ServiceKey> {
        self.selected_service().map(ServiceKey::from_service)
    }

    pub fn selected_row(&self) -> Option<ViewRow<'_>> {
        self.visible_rows().nth(self.selected)
    }

    pub fn current_view(&self) -> ViewMode {
        self.view_mode
    }

    pub fn current_view_label(&self) -> &'static str {
        self.view_mode.label()
    }

    pub fn visible_count(&self) -> usize {
        self.view_rows.len()
    }

    pub fn row_count(&self) -> usize {
        self.visible_count()
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = ViewRow<'_>> + '_ {
        self.view_rows.iter().filter_map(|row| match *row {
            RowIndex::Service(index) => self.services.get(index).map(ViewRow::Service),
            RowIndex::Connection {
                service_index,
                connection_index,
            } => self.services.get(service_index).and_then(|service| {
                service
                    .connections
                    .get(connection_index)
                    .map(|connection| ViewRow::Connection {
                        service,
                        connection,
                    })
            }),
        })
    }

    pub fn next_view(&mut self) {
        self.set_view_mode(self.view_mode.next());
    }

    pub fn previous_view(&mut self) {
        self.set_view_mode(self.view_mode.previous());
    }

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        if self.view_mode != mode {
            self.view_mode = mode;
            self.recompute_visible();
        }
    }

    pub fn push_history(&mut self, kind: HistoryKind, detail: String) {
        self.history.push_back(HistoryEvent::new(kind, detail));
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
    }

    pub fn recompute_visible(&mut self) {
        let selected_key = self
            .selected_row_key
            .clone()
            .or_else(|| self.selected_key.clone().map(RowKey::Service))
            .or_else(|| self.selected_row().and_then(|row| self.row_key(row)));
        let filter = if self.search_query.trim().is_empty() {
            Filter::default()
        } else {
            Filter::search(self.search_query.clone())
        };

        self.view_rows.clear();
        self.visible.clear();
        for (service_index, service) in self.services.iter().enumerate() {
            let service_matches = filter.matches(service);
            let include_service = service_matches
                && (self.view_mode == ViewMode::All
                    || (self.view_mode == ViewMode::Services && service.state.is_listening()));
            if include_service {
                self.view_rows.push(RowIndex::Service(service_index));
                self.visible.push(service_index);
            }

            if matches!(self.view_mode, ViewMode::Connections | ViewMode::All) {
                for (connection_index, connection) in service.connections.iter().enumerate() {
                    if connection.is_active() && filter.matches_connection(connection) {
                        self.view_rows.push(RowIndex::Connection {
                            service_index,
                            connection_index,
                        });
                    }
                }
            }
        }

        self.selected = selected_key
            .as_ref()
            .and_then(|key| {
                self.view_rows.iter().position(|row| {
                    self.row_key_from_index(*row)
                        .is_some_and(|candidate| candidate == *key)
                })
            })
            .unwrap_or_else(|| self.selected.min(self.row_count().saturating_sub(1)));
        self.sync_selection_keys();
    }

    fn row_key(&self, row: ViewRow<'_>) -> Option<RowKey> {
        match row {
            ViewRow::Service(service) => Some(RowKey::Service(ServiceKey::from_service(service))),
            ViewRow::Connection { connection, .. } => Some(RowKey::Connection(
                ConnectionKey::from_connection(connection),
            )),
        }
    }

    fn row_key_from_index(&self, row: RowIndex) -> Option<RowKey> {
        match row {
            RowIndex::Service(index) => self
                .services
                .get(index)
                .map(|service| RowKey::Service(ServiceKey::from_service(service))),
            RowIndex::Connection {
                service_index,
                connection_index,
            } => self
                .services
                .get(service_index)
                .and_then(|service| service.connections.get(connection_index))
                .map(|connection| RowKey::Connection(ConnectionKey::from_connection(connection))),
        }
    }

    fn sync_selection_keys(&mut self) {
        self.selected_row_key = self
            .view_rows
            .get(self.selected)
            .and_then(|row| self.row_key_from_index(*row));
        self.selected_key = self.selected_service_key();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.view_rows.is_empty() {
            self.selected = 0;
            self.selected_key = None;
            self.selected_row_key = None;
            return;
        }
        let max = self.view_rows.len() - 1;
        let target = (self.selected as isize + delta).clamp(0, max as isize) as usize;
        self.selected = target;
        self.sync_selection_keys();
    }

    /// Select a row by its position in the currently visible, filtered list.
    ///
    /// Pointer coordinates can become stale when the terminal is resized or
    /// data is refreshed between frames, so an out-of-range row is clamped to
    /// the last visible row. Empty lists reset to the same neutral state used
    /// by keyboard selection movement.
    pub(crate) fn select_visible_index(&mut self, index: usize) {
        if self.view_rows.is_empty() {
            self.selected = 0;
            self.selected_key = None;
            self.selected_row_key = None;
            return;
        }

        self.selected = index.min(self.view_rows.len() - 1);
        self.sync_selection_keys();
    }

    /// Move selection using the same clamping semantics as keyboard movement.
    pub(crate) fn move_selection_by(&mut self, delta: isize) {
        self.move_selection(delta);
    }

    pub fn select_home(&mut self) {
        self.selected = 0;
        self.sync_selection_keys();
    }

    pub fn select_end(&mut self) {
        self.selected = self.view_rows.len().saturating_sub(1);
        self.sync_selection_keys();
    }

    pub fn begin_search(&mut self) {
        self.search_input = self.search_query.clone();
        self.overlay = Overlay::Search;
    }

    pub fn toggle_inspection(&mut self) {
        self.focus = if self.focus == Focus::Inspection {
            Focus::Overview
        } else {
            Focus::Inspection
        };
    }

    pub fn show_binary_path(&mut self) {
        let Some(path) = self
            .selected_service()
            .and_then(|service| service.process.executable.as_deref())
            .map(|path| path.display().to_string())
        else {
            self.status = Some("selected process has no executable path".to_owned());
            return;
        };
        self.overlay = Overlay::BinaryPath(path);
        self.status = None;
    }

    pub fn request_confirmation(&mut self, kind: ConfirmKind) {
        if let Some(service) = self.selected_service() {
            let pid = service.process.pid;
            let blocked_reason = if pid <= 1 {
                Some(format!("refusing to terminate system process (PID {pid})"))
            } else if pid == std::process::id() {
                Some(format!(
                    "refusing to terminate the current ports process (PID {pid})"
                ))
            } else {
                None
            };

            let mut sockets: Vec<String> = self
                .services
                .iter()
                .filter(|s| s.process.pid == pid)
                .flat_map(|s| {
                    s.bindings
                        .iter()
                        .map(move |binding| format!("{} {}", s.protocol, binding))
                })
                .collect();
            sockets.sort();
            sockets.dedup();

            let connection_count: usize = self
                .services
                .iter()
                .filter(|s| s.process.pid == pid)
                .map(|s| s.connections.len())
                .sum();

            self.overlay = Overlay::Confirm(Box::new(Confirmation {
                kind,
                process: service.process.clone(),
                endpoint: service.local.to_string(),
                sockets,
                connection_count,
                blocked_reason,
                input: String::new(),
            }));
            self.status = None;
        } else {
            self.status = Some("nothing selected".to_owned());
        }
    }

    pub fn confirm(&mut self) -> Result<()> {
        let Overlay::Confirm(confirmation) = self.overlay.clone() else {
            return Ok(());
        };
        if let Some(reason) = &confirmation.blocked_reason {
            self.overlay = Overlay::None;
            self.error = Some(reason.clone());
            return Ok(());
        }
        self.overlay = Overlay::None;
        match terminate_pid(confirmation.process.pid, confirmation.kind.force()) {
            Ok(()) => {
                self.error = None;
                self.status = Some(format!(
                    "{} sent to {} (PID {})",
                    confirmation.kind.signal(),
                    confirmation.process.name,
                    confirmation.process.pid
                ));
                let _ = self.refresh();
                Ok(())
            }
            Err(error) => {
                let message = format!(
                    "could not signal {} (PID {}): {error}",
                    confirmation.process.name, confirmation.process.pid
                );
                self.error = Some(message);
                self.status = Some("terminate failed".to_owned());
                Err(error).context("terminate selected process")
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }
        match self.overlay.clone() {
            Overlay::Search => self.handle_search_key(key),
            Overlay::Confirm(_) => self.handle_confirm_key(key),
            Overlay::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.overlay = Overlay::None;
                }
                Ok(())
            }
            Overlay::BinaryPath(_) => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('p')) {
                    self.overlay = Overlay::None;
                }
                Ok(())
            }
            Overlay::None => self.handle_normal_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.search_input.clear();
                self.overlay = Overlay::None;
            }
            KeyCode::Enter => {
                self.search_query = self.search_input.trim().to_owned();
                self.overlay = Overlay::None;
                self.recompute_visible();
                self.status = if self.search_query.is_empty() {
                    Some("search cleared".to_owned())
                } else {
                    Some(format!("searching for {}", self.search_query))
                };
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.push(character);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Result<()> {
        let Overlay::Confirm(confirmation) = &self.overlay else {
            return Ok(());
        };

        if let Some(reason) = &confirmation.blocked_reason {
            if matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Char('q')
                    | KeyCode::Char('n')
                    | KeyCode::Char('N')
                    | KeyCode::Char('y')
                    | KeyCode::Char('Y')
            ) {
                let reason = reason.clone();
                self.overlay = Overlay::None;
                self.error = Some(reason);
            }
            return Ok(());
        }

        match confirmation.kind {
            ConfirmKind::Terminate => match key.code {
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.overlay = Overlay::None;
                    self.status = Some("action cancelled".to_owned());
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let _ = self.confirm();
                }
                _ => {}
            },
            ConfirmKind::Kill => match key.code {
                KeyCode::Esc => {
                    self.overlay = Overlay::None;
                    self.status = Some("action cancelled".to_owned());
                }
                KeyCode::Backspace => {
                    if let Overlay::Confirm(c) = &mut self.overlay {
                        c.input.pop();
                    }
                }
                KeyCode::Enter => {
                    if confirmation.input.trim() == "KILL" {
                        let _ = self.confirm();
                    } else {
                        self.status = Some("type KILL to confirm force-kill".to_owned());
                    }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Overlay::Confirm(c) = &mut self.overlay {
                        if c.input.len() < 16 {
                            c.input.push(character);
                        }
                    }
                }
                _ => {}
            },
        }
        Ok(())
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('/') => self.begin_search(),
            KeyCode::Left | KeyCode::Char('h') => self.previous_view(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('v') => self.next_view(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.select_home(),
            KeyCode::End => self.select_end(),
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::Enter => self.toggle_inspection(),
            KeyCode::Char('p') => self.show_binary_path(),
            KeyCode::Char('r') => {
                let _ = self.refresh();
            }
            KeyCode::Char('c') => self.copy_address(),
            KeyCode::Char('u') => self.copy_local_url(),
            KeyCode::Char('o') => self.open_http_service(),
            KeyCode::Char('x') => self.request_confirmation(ConfirmKind::Terminate),
            KeyCode::Char('X') => self.request_confirmation(ConfirmKind::Kill),
            _ => {}
        }
        Ok(())
    }

    fn copy_address(&mut self) {
        let Some(service) = self.selected_service() else {
            self.status = Some("nothing selected".to_owned());
            return;
        };
        let address = service.local.to_string();
        match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(address.clone()))
        {
            Ok(()) => self.status = Some(format!("copied {address}")),
            Err(error) => self.status = Some(format!("clipboard unavailable: {error}")),
        }
    }

    fn copy_local_url(&mut self) {
        let Some(service) = self.selected_service() else {
            self.status = Some("nothing selected".to_owned());
            return;
        };
        if !is_likely_http(service) {
            self.status = Some("selected service does not look like HTTP".to_owned());
            return;
        }
        let scheme = if matches!(service.local.port, 443 | 8443) {
            "https"
        } else {
            "http"
        };
        let url = format!("{scheme}://127.0.0.1:{}/", service.local.port);
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(url.clone())) {
            Ok(()) => self.status = Some(format!("copied {url}")),
            Err(error) => self.status = Some(format!("clipboard unavailable: {error}")),
        }
    }

    fn open_http_service(&mut self) {
        let Some(service) = self.selected_service() else {
            self.status = Some("nothing selected".to_owned());
            return;
        };
        if !is_likely_http(service) {
            self.status = Some("selected service does not look like HTTP".to_owned());
            return;
        }
        let scheme = if matches!(service.local.port, 443 | 8443) {
            "https"
        } else {
            "http"
        };
        let url = format!("{scheme}://127.0.0.1:{}/", service.local.port);
        match open::that(&url) {
            Ok(()) => self.status = Some(format!("opened {url}")),
            Err(error) => self.status = Some(format!("could not open {url}: {error}")),
        }
    }
}

fn service_label(service: &ServiceRecord) -> String {
    format!(
        "{} {} · {} (PID {})",
        service.protocol,
        service.local,
        service.service.as_deref().unwrap_or(&service.process.name),
        service.process.pid
    )
}

fn format_discovery_error(error: &anyhow::Error) -> String {
    let lower = error.to_string().to_lowercase();
    if lower.contains("permission") || lower.contains("operation not permitted") {
        format!("permission denied while inspecting sockets · {error}")
    } else {
        format!("discovery unavailable · {error}")
    }
}

fn is_likely_http(service: &ServiceRecord) -> bool {
    if matches!(
        service.local.port,
        80 | 443 | 3000 | 3001 | 4000 | 5000 | 5173 | 8000 | 8001 | 8080 | 8081 | 8443 | 8888
    ) {
        return true;
    }
    service.service.as_deref().is_some_and(|name| {
        let name = name.to_lowercase();
        ["http", "https", "web", "vite", "next", "rails", "django"]
            .iter()
            .any(|hint| name.contains(hint))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::model::{ConnectionRecord, Endpoint, ProcessMetadata, SocketState};
    use std::{
        net::{IpAddr, Ipv4Addr},
        path::PathBuf,
    };

    fn service(port: u16, pid: u32, state: SocketState) -> ServiceRecord {
        ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            state,
            ProcessMetadata::new(pid, format!("proc-{pid}")),
            None,
        )
    }

    fn connection(local_port: u16, pid: u32, remote_port: u16) -> ConnectionRecord {
        ConnectionRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local_port),
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), remote_port),
            SocketState::Established,
            ProcessMetadata::new(pid, format!("proc-{pid}")),
        )
    }

    #[test]
    fn services_is_the_default_view_and_views_cycle_deterministically() {
        let mut app = App::default();
        assert_eq!(app.view_mode, ViewMode::Services);
        assert_eq!(app.current_view_label(), "Services");

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.view_mode, ViewMode::Connections);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.view_mode, ViewMode::All);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.view_mode, ViewMode::Connections);
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.view_mode, ViewMode::All);
    }

    #[test]
    fn views_classify_services_and_active_connections() {
        let mut listener = service(8080, 7, SocketState::Listening);
        listener.add_connection(connection(8080, 7, 51000));
        let mut standalone = service(9090, 8, SocketState::Established);
        standalone.add_connection(connection(9090, 8, 52000));
        let app = App::from_services(vec![listener, standalone]);

        assert_eq!(app.visible_count(), 1);
        assert!(app
            .visible_rows()
            .all(|row| matches!(row, ViewRow::Service(_))));

        let mut app = app;
        app.set_view_mode(ViewMode::Connections);
        assert_eq!(app.row_count(), 2);
        assert!(app
            .visible_rows()
            .all(|row| matches!(row, ViewRow::Connection { .. })));
        assert!(app.visible_rows().any(|row| {
            matches!(
                row,
                ViewRow::Connection { connection, .. } if connection.remote.port == 52000
            )
        }));

        app.set_view_mode(ViewMode::All);
        assert_eq!(app.row_count(), 4);
        assert_eq!(
            app.visible_rows()
                .filter(|row| matches!(row, ViewRow::Service(_)))
                .count(),
            2
        );
        assert_eq!(
            app.visible_rows()
                .filter(|row| matches!(row, ViewRow::Connection { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn connection_filtering_is_conjunctive_and_uses_connection_fields() {
        let mut listener = service(8080, 7, SocketState::Listening);
        listener.add_connection(connection(8080, 7, 51000));
        let mut app = App::from_services(vec![listener]);
        app.set_view_mode(ViewMode::Connections);

        app.search_query = "51000 proc-7".to_owned();
        app.recompute_visible();
        assert_eq!(app.row_count(), 1);

        app.search_query = "51000 missing".to_owned();
        app.recompute_visible();
        assert_eq!(app.row_count(), 0);
    }

    #[test]
    fn selection_survives_binding_address_changes() {
        let selected = service(8080, 7, SocketState::Listening);
        let mut app = App::from_services(vec![selected]);
        let key = app.selected_service_key();
        app.replace_services(vec![ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 8080),
            SocketState::Listening,
            ProcessMetadata::new(7, "proc-7"),
            None,
        )]);
        assert_eq!(app.selected_service_key(), key);
        assert_eq!(
            app.selected_service().map(|service| service.local.address),
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)))
        );
    }

    #[test]
    fn selection_follows_stable_socket_identity_when_rows_reorder() {
        let mut app = App::from_services(vec![
            service(9000, 2, SocketState::Listening),
            service(8000, 1, SocketState::Listening),
        ]);
        app.select_end();
        let selected = app.selected_service_key();
        app.replace_services(vec![
            service(8000, 1, SocketState::Listening),
            service(9000, 2, SocketState::Listening),
        ]);
        assert_eq!(app.selected_service_key(), selected);
    }

    #[test]
    fn select_visible_index_tracks_filtered_rows_and_stable_identity() {
        let mut app = App::from_services(vec![
            service(9000, 2, SocketState::Listening),
            service(8000, 1, SocketState::Listening),
            service(7000, 3, SocketState::Listening),
        ]);

        app.search_query = "8000".to_owned();
        app.recompute_visible();
        assert_eq!(app.visible.len(), 1);
        app.select_visible_index(0);
        assert_eq!(
            app.selected_service().map(|service| service.local.port),
            Some(8000)
        );

        app.search_query.clear();
        app.recompute_visible();
        app.select_visible_index(1);
        let selected = app.selected_service_key();
        app.replace_services(vec![
            service(7000, 3, SocketState::Listening),
            service(9000, 2, SocketState::Listening),
            service(8000, 1, SocketState::Listening),
        ]);
        assert_eq!(app.selected_service_key(), selected);
    }

    #[test]
    fn select_visible_index_clamps_and_handles_empty_filtered_rows() {
        let mut app = App::from_services(vec![
            service(9000, 2, SocketState::Listening),
            service(8000, 1, SocketState::Listening),
            service(7000, 3, SocketState::Listening),
        ]);

        app.select_visible_index(usize::MAX);
        assert_eq!(app.selected, app.visible.len() - 1);
        assert_eq!(app.selected_key, app.selected_service_key());

        app.search_query = "missing".to_owned();
        app.recompute_visible();
        assert!(app.visible.is_empty());
        app.selected = 99;
        app.selected_key = Some(ServiceKey {
            protocol: Protocol::Tcp,
            port: 7000,
            pid: 3,
        });
        app.select_visible_index(0);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_key, None);
    }

    #[test]
    fn wheel_movement_uses_app_invariants() {
        let mut app = App::from_services(vec![
            service(9000, 2, SocketState::Listening),
            service(8000, 1, SocketState::Listening),
            service(7000, 3, SocketState::Listening),
        ]);

        app.move_selection_by(10);
        assert_eq!(app.selected, app.visible.len() - 1);
        app.move_selection_by(-10);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_key, app.selected_service_key());
    }

    #[test]
    fn history_records_open_close_and_process_exit_and_is_bounded() {
        let mut app = App::from_services(vec![service(8080, 7, SocketState::Listening)]);
        app.replace_services(vec![service(8080, 7, SocketState::Listening)]);
        assert!(app.history.is_empty());
        app.replace_services(Vec::new());
        assert!(app
            .history
            .iter()
            .any(|event| event.kind == HistoryKind::Closed));
        assert!(app
            .history
            .iter()
            .any(|event| event.kind == HistoryKind::ProcessExited));
        for number in 0..(HISTORY_LIMIT + 10) {
            app.push_history(HistoryKind::Opened, number.to_string());
        }
        assert_eq!(app.history.len(), HISTORY_LIMIT);
    }

    #[test]
    fn binary_path_overlay_reveals_full_executable_path() {
        let mut selected = service(8080, 7, SocketState::Listening);
        selected.process.executable = Some(PathBuf::from("/usr/libexec/remoted"));
        let mut app = App::from_services(vec![selected]);

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.overlay,
            Overlay::BinaryPath("/usr/libexec/remoted".to_owned())
        );

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn filtering_and_confirmation_are_gated() {
        let mut app = App::from_services(vec![
            service(8080, 7, SocketState::Listening),
            service(22, 8, SocketState::Listening),
        ]);
        app.search_query = "8080".to_owned();
        app.recompute_visible();
        assert_eq!(app.visible.len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn search_overlay_traps_navigation_until_enter() {
        let mut app = App::from_services(vec![service(8080, 7, SocketState::Listening)]);
        app.begin_search();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.selected, 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('8'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.search_query, "8");
    }

    #[test]
    fn request_confirmation_gathers_process_sockets_and_connections() {
        let mut proc = ProcessMetadata::new(42, "proc-42");
        proc.command = Some("node server.js".to_owned());
        proc.user = Some("developer".to_owned());

        let s1 = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            SocketState::Listening,
            proc.clone(),
            None,
        );
        let s2 = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000),
            SocketState::Listening,
            proc.clone(),
            None,
        );
        let mut s3 = ServiceRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
            SocketState::Listening,
            proc,
            None,
        );
        s3.add_connection(ports::model::ConnectionRecord::new(
            Protocol::Tcp,
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
            Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234),
            SocketState::Established,
            ProcessMetadata::new(42, "proc-42"),
        ));
        let mut app = App::from_services(vec![s1, s2, s3]);
        app.request_confirmation(ConfirmKind::Terminate);

        let Overlay::Confirm(confirmation) = &app.overlay else {
            panic!("expected Confirm overlay");
        };
        assert_eq!(confirmation.kind, ConfirmKind::Terminate);
        assert_eq!(confirmation.process.pid, 42);
        assert_eq!(confirmation.process.name, "proc-42");
        assert_eq!(
            confirmation.process.command.as_deref(),
            Some("node server.js")
        );
        assert_eq!(confirmation.sockets.len(), 3);
        assert_eq!(confirmation.connection_count, 1);
        assert!(confirmation.blocked_reason.is_none());
        assert!(!confirmation.is_blocked());
    }

    #[test]
    fn request_confirmation_blocks_system_pids_and_self() {
        let s_init = service(80, 1, SocketState::Listening);
        let mut app = App::from_services(vec![s_init]);
        app.request_confirmation(ConfirmKind::Terminate);

        let Overlay::Confirm(confirmation) = &app.overlay else {
            panic!("expected Confirm overlay");
        };
        assert!(confirmation.is_blocked());
        assert!(confirmation
            .blocked_reason
            .as_ref()
            .unwrap()
            .contains("system process"));

        // Dismissing a blocked confirmation overlay sets error
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.error.as_ref().unwrap().contains("system process"));

        // Test self PID
        let self_pid = std::process::id();
        let s_self = service(9999, self_pid, SocketState::Listening);
        let mut app_self = App::from_services(vec![s_self]);
        app_self.request_confirmation(ConfirmKind::Kill);
        let Overlay::Confirm(conf_self) = &app_self.overlay else {
            panic!("expected Confirm overlay");
        };
        assert!(conf_self.is_blocked());
        assert!(conf_self
            .blocked_reason
            .as_ref()
            .unwrap()
            .contains("current ports process"));
    }

    #[test]
    fn force_kill_requires_typing_kill_to_confirm() {
        let s = service(8080, 99999, SocketState::Listening);
        let mut app = App::from_services(vec![s]);
        app.request_confirmation(ConfirmKind::Kill);

        // Pressing Enter with empty input does not confirm
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.overlay, Overlay::Confirm(_)));
        assert_eq!(
            app.status.as_deref(),
            Some("type KILL to confirm force-kill")
        );

        // Type 'K', 'I', 'L'
        app.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.overlay, Overlay::Confirm(_)));

        // Backspace and type 'L' then another 'L' to make "KILL"
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE))
            .unwrap();

        let Overlay::Confirm(c) = &app.overlay else {
            panic!("expected Confirm overlay");
        };
        assert_eq!(c.input, "KILL");

        // Esc cancels
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.status.as_deref(), Some("action cancelled"));
    }

    #[test]
    fn terminate_confirmation_accepts_y_and_enter_and_cancels_on_n() {
        let s = service(8080, 99999, SocketState::Listening);
        let mut app = App::from_services(vec![s]);
        app.request_confirmation(ConfirmKind::Terminate);

        // 'n' cancels
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.status.as_deref(), Some("action cancelled"));
    }
}
