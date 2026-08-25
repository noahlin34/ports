use serde::{Deserialize, Serialize};

use crate::model::{
    ConnectionRecord, NetworkScope, Protocol, ServiceRecord, SocketRecord, SocketState,
};

/// Conjunctive criteria for the overview table and JSON output.
///
/// Text criteria (`process`, `address`, `cwd`, and `search`) are
/// case-insensitive substring matches. Boolean criteria are opt-in so the
/// default filter preserves the complete discovery result.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Filter {
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub process: Option<String>,
    pub protocol: Option<Protocol>,
    pub address: Option<String>,
    pub cwd: Option<String>,
    pub state: Option<SocketState>,
    pub scope: Option<NetworkScope>,
    pub current_user: bool,
    pub active_connection: bool,
    pub search: Option<String>,
}

impl Filter {
    pub fn search(query: impl Into<String>) -> Self {
        Self {
            search: Some(query.into()),
            ..Self::default()
        }
    }

    pub fn with_search(mut self, query: impl Into<String>) -> Self {
        self.search = Some(query.into());
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    pub fn with_process(mut self, process: impl Into<String>) -> Self {
        self.process = Some(process.into());
        self
    }

    pub fn with_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = Some(protocol);
        self
    }

    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_state(mut self, state: SocketState) -> Self {
        self.state = Some(state);
        self
    }

    pub fn with_scope(mut self, scope: NetworkScope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub fn for_current_user(mut self) -> Self {
        self.current_user = true;
        self
    }

    pub fn with_active_connection(mut self) -> Self {
        self.active_connection = true;
        self
    }

    /// Match a service row, including its peer connections.
    pub fn matches(&self, service: &ServiceRecord) -> bool {
        if !self.matches_process(&service.process)
            || !self.matches_protocol(service.protocol)
            || !self.matches_cwd(&service.process)
            || !self.matches_current_user(&service.process)
        {
            return false;
        }

        if self.active_connection && !service.has_active_connection() {
            return false;
        }

        if let Some(scope) = self.scope {
            let scope_matches = service.scope == scope
                || service
                    .connections
                    .iter()
                    .any(|connection| connection.scope == scope);
            if !scope_matches {
                return false;
            }
        }

        if let Some(state) = &self.state {
            let state_matches = service.state == *state
                || service
                    .connections
                    .iter()
                    .any(|connection| connection.state == *state);
            if !state_matches {
                return false;
            }
        }

        if let Some(port) = self.port {
            let port_matches = service.local.port == port
                || service.connections.iter().any(|connection| {
                    connection.local.port == port || connection.remote.port == port
                });
            if !port_matches {
                return false;
            }
        }

        if let Some(address) = &self.address {
            let address_matches = endpoint_contains(&service.local, address)
                || service.connections.iter().any(|connection| {
                    endpoint_contains(&connection.local, address)
                        || endpoint_contains(&connection.remote, address)
                });
            if !address_matches {
                return false;
            }
        }

        self.matches_search(&service.overview())
    }

    /// Match a single discovered socket. This is useful to discovery backends
    /// before they have grouped sockets into service rows.
    pub fn matches_socket(&self, socket: &SocketRecord) -> bool {
        if !self.matches_process(&socket.process)
            || !self.matches_protocol(socket.protocol)
            || !self.matches_cwd(&socket.process)
            || !self.matches_current_user(&socket.process)
        {
            return false;
        }
        if self.active_connection && !socket.is_connection() {
            return false;
        }
        if let Some(port) = self.port {
            if socket.local.port != port
                && socket
                    .remote
                    .as_ref()
                    .is_none_or(|remote| remote.port != port)
            {
                return false;
            }
        }
        if let Some(scope) = self.scope {
            if socket.scope != scope {
                return false;
            }
        }
        if let Some(state) = &self.state {
            if socket.state != *state {
                return false;
            }
        }
        if let Some(address) = &self.address {
            if !endpoint_contains(&socket.local, address)
                && socket
                    .remote
                    .as_ref()
                    .is_none_or(|remote| !endpoint_contains(remote, address))
            {
                return false;
            }
        }
        self.matches_search(&socket.overview())
    }

    /// Match a connected peer independently of its parent service.
    pub fn matches_connection(&self, connection: &ConnectionRecord) -> bool {
        if !self.matches_process(&connection.process)
            || !self.matches_protocol(connection.protocol)
            || !self.matches_cwd(&connection.process)
            || !self.matches_current_user(&connection.process)
        {
            return false;
        }
        if self.active_connection && !connection.is_active() {
            return false;
        }
        if let Some(port) = self.port {
            if connection.local.port != port && connection.remote.port != port {
                return false;
            }
        }
        if let Some(scope) = self.scope {
            if connection.scope != scope {
                return false;
            }
        }
        if let Some(state) = &self.state {
            if connection.state != *state {
                return false;
            }
        }
        if let Some(address) = &self.address {
            if !endpoint_contains(&connection.local, address)
                && !endpoint_contains(&connection.remote, address)
            {
                return false;
            }
        }
        self.matches_search(&connection.overview())
    }

    pub fn apply<'a>(&self, services: &'a [ServiceRecord]) -> Vec<&'a ServiceRecord> {
        services
            .iter()
            .filter(|service| self.matches(service))
            .collect()
    }

    pub fn filter<'a, I>(&self, services: I) -> Vec<&'a ServiceRecord>
    where
        I: IntoIterator<Item = &'a ServiceRecord>,
    {
        services
            .into_iter()
            .filter(|service| self.matches(service))
            .collect()
    }

    fn matches_process(&self, process: &crate::model::ProcessMetadata) -> bool {
        self.process.as_deref().is_none_or(|needle| {
            contains_folded(&process.name, needle)
                || process
                    .command
                    .as_deref()
                    .is_some_and(|command| contains_folded(command, needle))
        }) && self.pid.is_none_or(|pid| process.pid == pid)
    }

    fn matches_protocol(&self, protocol: Protocol) -> bool {
        self.protocol.is_none_or(|wanted| wanted == protocol)
    }

    fn matches_cwd(&self, process: &crate::model::ProcessMetadata) -> bool {
        self.cwd.as_deref().is_none_or(|needle| {
            process
                .cwd
                .as_deref()
                .is_some_and(|cwd| contains_folded(&cwd.display().to_string(), needle))
        })
    }

    fn matches_current_user(&self, process: &crate::model::ProcessMetadata) -> bool {
        !self.current_user || process.is_current_user
    }

    fn matches_search(&self, overview: &str) -> bool {
        self.search.as_deref().is_none_or(|query| {
            let folded = overview.to_lowercase();
            query
                .split_whitespace()
                .all(|term| folded.contains(&term.to_lowercase()))
        })
    }
}

fn endpoint_contains(endpoint: &crate::model::Endpoint, needle: &str) -> bool {
    contains_folded(&endpoint.address.to_string(), needle)
        || endpoint.port.to_string() == needle.trim()
}

fn contains_folded(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.to_lowercase())
}

/// A reusable AND-composition of independently built filters.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilterSet {
    pub filters: Vec<Filter>,
}

impl FilterSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_filters(filters: impl IntoIterator<Item = Filter>) -> Self {
        Self {
            filters: filters.into_iter().collect(),
        }
    }

    pub fn push(&mut self, filter: Filter) {
        self.filters.push(filter);
    }

    pub fn matches(&self, service: &ServiceRecord) -> bool {
        self.filters.iter().all(|filter| filter.matches(service))
    }

    pub fn apply<'a>(&self, services: &'a [ServiceRecord]) -> Vec<&'a ServiceRecord> {
        services
            .iter()
            .filter(|service| self.matches(service))
            .collect()
    }
}
