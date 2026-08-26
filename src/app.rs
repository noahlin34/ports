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
    model::{Protocol, ServiceRecord},
};

const HISTORY_LIMIT: usize = 128;
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServiceKey {
    pub protocol: Protocol,
    pub address: std::net::IpAddr,
    pub port: u16,
    pub pid: u32,
}

impl ServiceKey {
    pub fn from_service(service: &ServiceRecord) -> Self {
        Self {
            protocol: service.protocol,
            address: service.local.address,
            port: service.local.port,
            pid: service.process.pid,
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
    pub process: String,
    pub pid: u32,
    pub endpoint: String,
}

impl Confirmation {
    pub fn prompt(&self) -> String {
        format!(
            "Send {} to {} (PID {}) on {}?",
            self.kind.signal(),
            self.process,
            self.pid,
            self.endpoint
        )
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Overlay {
    None,
    Search,
    Help,
    BinaryPath(String),
    Confirm(Confirmation),
}

impl Default for Overlay {
    fn default() -> Self {
        Self::None
    }
}

/// All mutable UI state lives here. Rendering is intentionally a pure view of
/// this value; refreshes can therefore replace discovery records without
/// invalidating a selected socket or leaving stale modal state behind.
pub struct App {
    pub services: Vec<ServiceRecord>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub selected_key: Option<ServiceKey>,
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

impl Default for App {
    fn default() -> Self {
        Self {
            services: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            selected_key: None,
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
        self.visible
            .get(self.selected)
            .and_then(|index| self.services.get(*index))
    }

    pub fn selected_service_key(&self) -> Option<ServiceKey> {
        self.selected_service().map(ServiceKey::from_service)
    }

    pub fn push_history(&mut self, kind: HistoryKind, detail: String) {
        self.history.push_back(HistoryEvent::new(kind, detail));
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
    }

    pub fn recompute_visible(&mut self) {
        let selected_key = self
            .selected_key
            .clone()
            .or_else(|| self.selected_service_key());
        let filter = if self.search_query.trim().is_empty() {
            Filter::default()
        } else {
            Filter::search(self.search_query.clone())
        };
        self.visible = self
            .services
            .iter()
            .enumerate()
            .filter_map(|(index, service)| filter.matches(service).then_some(index))
            .collect();
        self.selected = selected_key
            .as_ref()
            .and_then(|key| {
                self.visible.iter().position(|index| {
                    self.services
                        .get(*index)
                        .is_some_and(|service| ServiceKey::from_service(service) == *key)
                })
            })
            .unwrap_or_else(|| self.selected.min(self.visible.len().saturating_sub(1)));
        self.selected_key = self.selected_service_key();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            self.selected = 0;
            self.selected_key = None;
            return;
        }
        let max = self.visible.len() - 1;
        let target = (self.selected as isize + delta).clamp(0, max as isize) as usize;
        self.selected = target;
        self.selected_key = self.selected_service_key();
    }

    pub fn select_home(&mut self) {
        self.selected = 0;
        self.selected_key = self.selected_service_key();
    }

    pub fn select_end(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
        self.selected_key = self.selected_service_key();
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
            self.overlay = Overlay::Confirm(Confirmation {
                kind,
                process: service.process.name.clone(),
                pid: service.process.pid,
                endpoint: service.local.to_string(),
            });
            self.status = None;
        } else {
            self.status = Some("nothing selected".to_owned());
        }
    }

    pub fn confirm(&mut self) -> Result<()> {
        let Overlay::Confirm(confirmation) = self.overlay.clone() else {
            return Ok(());
        };
        self.overlay = Overlay::None;
        match terminate_pid(confirmation.pid, confirmation.kind.force()) {
            Ok(()) => {
                self.status = Some(format!(
                    "{} sent to {} (PID {})",
                    confirmation.kind.signal(),
                    confirmation.process,
                    confirmation.pid
                ));
                let _ = self.refresh();
                Ok(())
            }
            Err(error) => {
                self.status = Some(format!(
                    "could not signal PID {}: {error}",
                    confirmation.pid
                ));
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
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.overlay = Overlay::None;
                self.status = Some("action cancelled".to_owned());
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let _ = self.confirm();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Char('/') => self.begin_search(),
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
    use ports::model::{Endpoint, ProcessMetadata, SocketState};
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
            service(9000, 2, SocketState::Established),
        ]);
        assert_eq!(app.selected_service_key(), selected);
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
}
