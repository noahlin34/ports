use std::{
    cmp::Ordering,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

/// A transport protocol understood by the discovery backends.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
}

impl Protocol {
    pub const ALL: [Self; 2] = [Self::Tcp, Self::Udp];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// States reported by TCP and UDP socket discovery.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketState {
    Listening,
    Established,
    SynSent,
    SynReceived,
    FinWait1,
    FinWait2,
    TimeWait,
    Close,
    CloseWait,
    LastAck,
    Closing,
    Bound,
    Unconnected,
    /// A platform-specific state that is not represented by the portable set.
    Other(String),
}

impl Default for SocketState {
    fn default() -> Self {
        Self::Listening
    }
}

impl SocketState {
    pub const fn is_listening(&self) -> bool {
        matches!(self, Self::Listening | Self::Bound)
    }

    /// Whether this state denotes a live, peer-bearing connection.
    pub const fn is_active_connection(&self) -> bool {
        matches!(
            self,
            Self::Established
                | Self::SynSent
                | Self::SynReceived
                | Self::FinWait1
                | Self::FinWait2
                | Self::CloseWait
                | Self::LastAck
                | Self::Closing
        )
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::TimeWait | Self::Close)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Listening => "LISTEN",
            Self::Established => "ESTABLISHED",
            Self::SynSent => "SYN-SENT",
            Self::SynReceived => "SYN-RECEIVED",
            Self::FinWait1 => "FIN-WAIT-1",
            Self::FinWait2 => "FIN-WAIT-2",
            Self::TimeWait => "TIME-WAIT",
            Self::Close => "CLOSED",
            Self::CloseWait => "CLOSE-WAIT",
            Self::LastAck => "LAST-ACK",
            Self::Closing => "CLOSING",
            Self::Bound => "BOUND",
            Self::Unconnected => "UNCONNECTED",
            Self::Other(value) => value.as_str(),
        }
    }
}

impl fmt::Display for SocketState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A conservative classification of the address a service is bound to.
///
/// The ordering is intentional: broad exposure sorts before narrower scopes in
/// table views. `External` means a concrete, public address on one interface;
/// it does not claim that a firewall or NAT will permit ingress.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum NetworkScope {
    /// A wildcard address (`0.0.0.0` or `::`) that binds every interface.
    #[default]
    AllInterfaces,
    /// A concrete publicly routable address on one interface.
    External,
    /// An RFC 1918 or IPv6 unique-local address.
    Private,
    /// An address in Tailscale's IPv4 CGNAT or IPv6 ULA range.
    Tailscale,
    /// An RFC 3927 / IPv6 link-local address.
    LinkLocal,
    /// An address reachable only from the local host.
    Loopback,
}

impl NetworkScope {
    pub fn classify(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(address) => Self::classify_v4(address),
            IpAddr::V6(address) => Self::classify_v6(address),
        }
    }

    pub fn from_ip(address: IpAddr) -> Self {
        Self::classify(address)
    }

    fn classify_v4(address: Ipv4Addr) -> Self {
        if address.is_unspecified() {
            Self::AllInterfaces
        } else if address.is_loopback() {
            Self::Loopback
        } else if is_tailscale_v4(address) {
            Self::Tailscale
        } else if address.is_link_local() {
            Self::LinkLocal
        } else if address.is_private() {
            Self::Private
        } else {
            Self::External
        }
    }

    fn classify_v6(address: Ipv6Addr) -> Self {
        if address.is_unspecified() {
            Self::AllInterfaces
        } else if address.is_loopback() {
            Self::Loopback
        } else if is_tailscale_v6(address) {
            Self::Tailscale
        } else if address.is_unicast_link_local() {
            Self::LinkLocal
        } else if address.is_unique_local() {
            Self::Private
        } else {
            Self::External
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::AllInterfaces => "all interfaces",
            Self::External => "external",
            Self::Private => "private / LAN",
            Self::Tailscale => "Tailscale",
            Self::LinkLocal => "link-local",
            Self::Loopback => "loopback",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::AllInterfaces => "listens on every IPv4/IPv6 interface",
            Self::External => "binds a public address on one interface",
            Self::Private => "reachable on a private or LAN interface",
            Self::Tailscale => "reachable through a Tailscale interface",
            Self::LinkLocal => "reachable only on the local network segment",
            Self::Loopback => "reachable only from this machine",
        }
    }

    pub const fn is_specific_interface(self) -> bool {
        !matches!(self, Self::AllInterfaces)
    }
}

impl fmt::Display for NetworkScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

fn is_tailscale_v4(address: Ipv4Addr) -> bool {
    let value = u32::from(address);
    (0x6440_0000..=0x647f_ffff).contains(&value)
}

fn is_tailscale_v6(address: Ipv6Addr) -> bool {
    // Tailscale's stable IPv6 ULA prefix is fd7a:115c:a1e0::/48.
    let segments = address.segments();
    segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
}

/// Metadata for the process that owns a socket.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ProcessMetadata {
    pub pid: u32,
    /// The short executable name used in compact views.
    pub name: String,
    /// The full executable path when the platform exposes it.
    pub executable: Option<PathBuf>,
    pub command: Option<String>,
    pub cwd: Option<PathBuf>,
    pub user: Option<String>,
    pub is_current_user: bool,
}

impl ProcessMetadata {
    pub fn new(pid: u32, name: impl Into<String>) -> Self {
        Self {
            pid,
            name: name.into(),
            ..Self::default()
        }
    }

    pub const fn current_user(&self) -> bool {
        self.is_current_user
    }

    pub fn overview(&self) -> String {
        let mut fields = vec![self.name.clone(), self.pid.to_string()];
        if let Some(command) = &self.command {
            fields.push(command.clone());
        }
        if let Some(cwd) = &self.cwd {
            fields.push(cwd.display().to_string());
        }
        if let Some(user) = &self.user {
            fields.push(user.clone());
        }
        fields.join(" ")
    }
}

impl fmt::Display for ProcessMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "pid {}", self.pid)
        } else {
            write!(f, "{} ({})", self.name, self.pid)
        }
    }
}

/// A typed IP endpoint. Keeping the address and port separate avoids the
/// parsing and display ambiguity of a single `"host:port"` string.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Endpoint {
    pub address: IpAddr,
    pub port: u16,
}

impl Endpoint {
    pub const fn new(address: IpAddr, port: u16) -> Self {
        Self { address, port }
    }

    pub const fn is_wildcard(&self) -> bool {
        self.address.is_unspecified()
    }

    pub fn scope(&self) -> NetworkScope {
        NetworkScope::classify(self.address)
    }
}

impl From<std::net::SocketAddr> for Endpoint {
    fn from(address: std::net::SocketAddr) -> Self {
        Self::new(address.ip(), address.port())
    }
}

impl From<Endpoint> for std::net::SocketAddr {
    fn from(endpoint: Endpoint) -> Self {
        std::net::SocketAddr::new(endpoint.address, endpoint.port)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.address {
            IpAddr::V4(address) => write!(f, "{address}:{}", self.port),
            IpAddr::V6(address) => write!(f, "[{address}]:{}", self.port),
        }
    }
}

/// Semantic aliases make ownership direction explicit at call sites while
/// retaining one compact wire representation.
pub type LocalEndpoint = Endpoint;
pub type RemoteEndpoint = Endpoint;

/// One discovered socket, optionally associated with a peer.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SocketRecord {
    pub protocol: Protocol,
    pub local: LocalEndpoint,
    pub remote: Option<RemoteEndpoint>,
    pub state: SocketState,
    pub scope: NetworkScope,
    pub process: ProcessMetadata,
}

impl SocketRecord {
    pub fn new(
        protocol: Protocol,
        local: LocalEndpoint,
        state: SocketState,
        process: ProcessMetadata,
    ) -> Self {
        let scope = local.scope();
        Self {
            protocol,
            local,
            remote: None,
            state,
            scope,
            process,
        }
    }

    pub fn with_remote(mut self, remote: RemoteEndpoint) -> Self {
        self.remote = Some(remote);
        self
    }

    pub fn is_connection(&self) -> bool {
        self.remote.is_some() && self.state.is_active_connection()
    }

    pub fn overview(&self) -> String {
        let remote = self
            .remote
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        format!(
            "{} {} {} {} {} {} {}",
            self.protocol,
            self.local,
            remote,
            self.state,
            self.scope,
            self.process,
            self.process
                .cwd
                .as_deref()
                .map_or_else(String::new, |path| path.display().to_string())
        )
    }
}

impl fmt::Display for SocketRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} ({})", self.protocol, self.local, self.state)
    }
}

/// A service/listener row shown in the overview table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceRecord {
    pub protocol: Protocol,
    pub local: LocalEndpoint,
    pub state: SocketState,
    pub scope: NetworkScope,
    pub process: ProcessMetadata,
    pub service: Option<String>,
    pub connections: Vec<ConnectionRecord>,
}

impl ServiceRecord {
    pub fn new(
        protocol: Protocol,
        local: LocalEndpoint,
        state: SocketState,
        process: ProcessMetadata,
        service: Option<String>,
    ) -> Self {
        let scope = local.scope();
        Self {
            protocol,
            local,
            state,
            scope,
            process,
            service,
            connections: Vec::new(),
        }
    }

    pub fn socket(&self) -> SocketRecord {
        SocketRecord::new(
            self.protocol,
            self.local.clone(),
            self.state.clone(),
            self.process.clone(),
        )
    }

    pub fn add_connection(&mut self, connection: ConnectionRecord) {
        self.connections.push(connection);
    }

    pub fn has_active_connection(&self) -> bool {
        self.connections.iter().any(ConnectionRecord::is_active)
    }

    pub fn active_connections(&self) -> impl Iterator<Item = &ConnectionRecord> {
        self.connections
            .iter()
            .filter(|connection| connection.is_active())
    }

    pub fn overview(&self) -> String {
        let connections = self
            .connections
            .iter()
            .map(ConnectionRecord::overview)
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{} {} {} {} {} {} {} {} {}",
            self.protocol,
            self.local,
            self.state,
            self.scope,
            self.service.as_deref().unwrap_or_default(),
            self.process,
            self.process.overview(),
            self.connections.len(),
            connections
        )
    }
}

impl Ord for ServiceRecord {
    fn cmp(&self, other: &Self) -> Ordering {
        self.local
            .port
            .cmp(&other.local.port)
            .then_with(|| self.protocol.cmp(&other.protocol))
            .then_with(|| self.local.cmp(&other.local))
            .then_with(|| self.scope.cmp(&other.scope))
            .then_with(|| self.state.cmp(&other.state))
            .then_with(|| self.process.cmp(&other.process))
            .then_with(|| self.service.cmp(&other.service))
            .then_with(|| self.connections.cmp(&other.connections))
    }
}

impl PartialOrd for ServiceRecord {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ServiceRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(service) = &self.service {
            write!(f, "{} {} ({service})", self.protocol, self.local)
        } else {
            write!(f, "{} {}", self.protocol, self.local)
        }
    }
}

/// A peer-bearing socket associated with a service.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ConnectionRecord {
    pub protocol: Protocol,
    pub local: LocalEndpoint,
    pub remote: RemoteEndpoint,
    pub state: SocketState,
    pub scope: NetworkScope,
    pub process: ProcessMetadata,
}

impl ConnectionRecord {
    pub fn new(
        protocol: Protocol,
        local: LocalEndpoint,
        remote: RemoteEndpoint,
        state: SocketState,
        process: ProcessMetadata,
    ) -> Self {
        let scope = local.scope();
        Self {
            protocol,
            local,
            remote,
            state,
            scope,
            process,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.is_active_connection()
    }

    pub fn overview(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            self.protocol,
            self.local,
            self.remote,
            self.state,
            self.scope,
            self.process,
            self.process
                .cwd
                .as_deref()
                .map_or_else(String::new, |path| path.display().to_string())
        )
    }
}

impl fmt::Display for ConnectionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} -> {} ({})",
            self.protocol, self.local, self.remote, self.state
        )
    }
}
