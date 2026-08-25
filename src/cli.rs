use std::fmt;
use std::io::{self, IsTerminal, Write};

use anyhow::anyhow;
use clap::{Args, Parser, Subcommand};
use ports::filter::Filter;
use ports::model::{
    ConnectionRecord, NetworkScope, ProcessMetadata, Protocol, ServiceRecord, SocketState,
};
use serde::Serialize;

/// Command-line entry point for Ports.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "ports",
    version,
    about = "Inspect local ports, listeners, processes, and connections",
    arg_required_else_help = false,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Show discovered listeners.
    List(ReadArgs),
    /// Inspect the owners and connections associated with a port.
    Inspect(InspectArgs),
    /// Show listeners owned by a process.
    Process(ProcessArgs),
    /// Show peer-bearing network connections.
    Connections(ReadArgs),
    /// Terminate the unique process listening on a port.
    Kill(KillArgs),
}

#[derive(Clone, Debug, Args)]
pub struct ReadArgs {
    #[command(flatten)]
    pub filters: Filters,
    /// Emit records as JSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct InspectArgs {
    /// Local port to inspect.
    pub port: u16,
    #[command(flatten)]
    pub filters: Filters,
    /// Emit records as JSON instead of a human-readable detail view.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ProcessArgs {
    /// Process ID to inspect.
    pub pid: u32,
    #[command(flatten)]
    pub filters: Filters,
    /// Emit records as JSON instead of a human-readable detail view.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct KillArgs {
    /// Local listening port whose owner should be terminated.
    pub port: u16,
    /// Do not ask for an interactive confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Send SIGKILL instead of the graceful termination signal.
    #[arg(long)]
    pub force: bool,
}

/// Filters shared by every read-only command.
///
/// The struct deliberately contains only command-line concerns. `to_filter` converts it to
/// the shared model filter, keeping the model independent from Clap and terminal concerns.
#[derive(Clone, Debug, Default, Args, PartialEq, Eq)]
pub struct Filters {
    /// Transport protocol (`tcp` or `udp`).
    #[arg(long, value_parser = parse_protocol)]
    pub protocol: Option<Protocol>,
    /// Socket state, for example `listening` or `established`.
    #[arg(long, value_parser = parse_state)]
    pub state: Option<SocketState>,
    /// Network exposure scope (`all-interfaces`, `external`, `private`, `tailscale`, `link-local`, or `loopback`).
    #[arg(long, value_parser = parse_scope)]
    pub scope: Option<NetworkScope>,
    /// Case-insensitive process-name or command substring.
    #[arg(long)]
    pub process: Option<String>,
    /// Exact process ID filter.
    #[arg(long, visible_alias = "process-id")]
    pub pid: Option<u32>,
    /// Exact owning username.
    #[arg(long)]
    pub user: Option<String>,
    /// Restrict rows to processes owned by the invoking user.
    #[arg(long)]
    pub current_user: bool,
    /// Restrict rows to services with active peer connections.
    #[arg(long, visible_alias = "active")]
    pub active_connections: bool,
    /// Include rows that are not listeners (and inactive connection states).
    #[arg(long, short = 'a')]
    pub all: bool,
    /// Match a local or remote address (or port) substring.
    #[arg(long)]
    pub address: Option<String>,
    /// Match the owning process current working directory.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Case-insensitive free-text search across the rendered record.
    #[arg(long, short = 's')]
    pub search: Option<String>,
}

impl Filters {
    pub fn to_filter(&self) -> Filter {
        let mut filter = Filter::default();
        filter.protocol = self.protocol;
        filter.state = self.state.clone();
        filter.scope = self.scope;
        filter.process = self.process.clone();
        filter.pid = self.pid;
        filter.address = self.address.clone();
        filter.cwd = self.cwd.clone();
        filter.search = self.search.clone();
        filter.current_user = self.current_user;
        filter.active_connection = self.active_connections;
        filter
    }

    fn matches_user(&self, process: &ProcessMetadata) -> bool {
        self.user
            .as_deref()
            .is_none_or(|user| process.user.as_deref() == Some(user))
    }

    fn matches_service(&self, service: &ServiceRecord) -> bool {
        self.matches_user(&service.process) && self.to_filter().matches(service)
    }

    fn matches_connection(&self, connection: &ConnectionRecord) -> bool {
        self.matches_user(&connection.process) && self.to_filter().matches_connection(connection)
    }
}

/// Convert a CLI protocol value to the shared model enum.
pub fn parse_protocol(value: &str) -> Result<Protocol, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "tcp" => Ok(Protocol::Tcp),
        "udp" => Ok(Protocol::Udp),
        _ => Err(format!("invalid protocol '{value}'; expected tcp or udp")),
    }
}

/// Convert a CLI state value to the shared model enum.
pub fn parse_state(value: &str) -> Result<SocketState, String> {
    let normalized = value.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let state = match normalized.as_str() {
        "listening" | "listen" => SocketState::Listening,
        "established" => SocketState::Established,
        "syn-sent" => SocketState::SynSent,
        "syn-received" => SocketState::SynReceived,
        "fin-wait-1" => SocketState::FinWait1,
        "fin-wait-2" => SocketState::FinWait2,
        "time-wait" => SocketState::TimeWait,
        "close" | "closed" => SocketState::Close,
        "close-wait" => SocketState::CloseWait,
        "last-ack" => SocketState::LastAck,
        "closing" => SocketState::Closing,
        "bound" => SocketState::Bound,
        "unconnected" => SocketState::Unconnected,
        other if !other.is_empty() => SocketState::Other(value.trim().to_string()),
        _ => return Err("state cannot be empty".to_string()),
    };
    Ok(state)
}

/// Convert a CLI scope value to the shared model enum.
pub fn parse_scope(value: &str) -> Result<NetworkScope, String> {
    match value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-")
        .as_str()
    {
        "all-interfaces" | "all" | "wildcard" | "global" => Ok(NetworkScope::AllInterfaces),
        "external" | "public" => Ok(NetworkScope::External),
        "private" | "lan" => Ok(NetworkScope::Private),
        "tailscale" => Ok(NetworkScope::Tailscale),
        "link-local" | "linklocal" => Ok(NetworkScope::LinkLocal),
        "loopback" | "local" => Ok(NetworkScope::Loopback),
        _ => Err(format!("invalid scope '{value}'")),
    }
}

#[derive(Debug)]
pub enum CliError {
    Discovery(anyhow::Error),
    NoMatches {
        what: String,
    },
    Ambiguous {
        port: u16,
        owners: Vec<ProcessMetadata>,
    },
    ConfirmationRequired,
    Cancelled,
    Io(anyhow::Error),
}

impl CliError {
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Ambiguous { .. } | Self::ConfirmationRequired | Self::Cancelled => 2,
            Self::NoMatches { .. } | Self::Discovery(_) | Self::Io(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(error) => write!(f, "discovery failed: {error}"),
            Self::NoMatches { what } => write!(f, "no matching {what} found"),
            Self::Ambiguous { port, owners } => {
                write!(f, "port {port} has multiple owners; refusing to guess")?;
                for owner in owners {
                    write!(f, "\n  - {owner}")?;
                }
                Ok(())
            }
            Self::ConfirmationRequired => {
                f.write_str("kill requires an interactive confirmation; pass --yes when automation is intentional")
            }
            Self::Cancelled => f.write_str("kill cancelled"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discovery(error) | Self::Io(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// Execute a parsed command. The no-subcommand case belongs to `main`, which starts the TUI.
pub fn run(cli: Cli) -> Result<(), CliError> {
    let Some(command) = cli.command else {
        return Ok(());
    };
    match command {
        Command::List(args) => run_list(&args),
        Command::Inspect(args) => run_inspect(&args),
        Command::Process(args) => run_process(&args),
        Command::Connections(args) => run_connections(&args),
        Command::Kill(args) => run_kill(&args),
    }
}

fn discover() -> Result<Vec<ServiceRecord>, CliError> {
    ports::discovery::discover().map_err(CliError::Discovery)
}

fn run_list(args: &ReadArgs) -> Result<(), CliError> {
    let services = discover()?;
    let rows = filter_services(&services, &args.filters, None);
    if rows.is_empty() {
        return Err(CliError::NoMatches {
            what: "listeners".into(),
        });
    }
    if args.json {
        print_json(&rows)?;
    } else {
        print_service_table(&rows);
    }
    Ok(())
}

fn run_inspect(args: &InspectArgs) -> Result<(), CliError> {
    let services = discover()?;
    let rows = filter_services(&services, &args.filters, Some(args.port));
    if rows.is_empty() {
        return Err(CliError::NoMatches {
            what: format!("owner for port {}", args.port),
        });
    }
    if args.json {
        print_json(&rows)?;
    } else {
        print_service_details(&rows, format!("Port {}", args.port));
    }
    Ok(())
}

fn run_process(args: &ProcessArgs) -> Result<(), CliError> {
    let services = discover()?;
    let mut filters = args.filters.clone();
    filters.pid = Some(args.pid);
    let rows = filter_services(&services, &filters, None);
    if rows.is_empty() {
        return Err(CliError::NoMatches {
            what: format!("process {}", args.pid),
        });
    }
    if args.json {
        print_json(&rows)?;
    } else {
        print_service_details(&rows, format!("Process {}", args.pid));
    }
    Ok(())
}

fn run_connections(args: &ReadArgs) -> Result<(), CliError> {
    let services = discover()?;
    let rows = filter_connections(&services, &args.filters);
    if rows.is_empty() {
        return Err(CliError::NoMatches {
            what: "connections".into(),
        });
    }
    if args.json {
        print_json(&rows)?;
    } else {
        print_connection_table(&rows);
    }
    Ok(())
}

fn run_kill(args: &KillArgs) -> Result<(), CliError> {
    let services = discover()?;
    let targets = resolve_kill_targets(&services, args.port);
    let Some(target) = targets.first() else {
        return Err(CliError::NoMatches {
            what: format!("listener on port {}", args.port),
        });
    };
    if targets.len() > 1 {
        return Err(CliError::Ambiguous {
            port: args.port,
            owners: targets.into_iter().map(|target| target.process).collect(),
        });
    }

    print_kill_context(target, args.force);
    if !args.yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(CliError::ConfirmationRequired);
        }
        if !confirm_kill(
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
            target,
            args.force,
        )? {
            return Err(CliError::Cancelled);
        }
    }
    ports::discovery::terminate_pid(target.process.pid, args.force).map_err(CliError::Discovery)?;
    println!(
        "terminated {} on port {}{}",
        target.process,
        args.port,
        if args.force { " (force)" } else { "" }
    );
    Ok(())
}

/// A process owner and all listener rows it owns on one port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KillTarget {
    pub process: ProcessMetadata,
    pub services: Vec<ServiceRecord>,
}

pub fn resolve_kill_targets(services: &[ServiceRecord], port: u16) -> Vec<KillTarget> {
    let mut targets: Vec<KillTarget> = Vec::new();
    for service in services
        .iter()
        .filter(|service| service.local.port == port && service.state.is_listening())
    {
        if let Some(target) = targets
            .iter_mut()
            .find(|target| target.process.pid == service.process.pid)
        {
            target.services.push(service.clone());
        } else {
            targets.push(KillTarget {
                process: service.process.clone(),
                services: vec![service.clone()],
            });
        }
    }
    targets.sort_by(|left, right| left.process.pid.cmp(&right.process.pid));
    targets
}

/// Apply service filters while preserving discovery order. `port` is an exact local-port match.
pub fn filter_services<'a>(
    services: &'a [ServiceRecord],
    filters: &Filters,
    port: Option<u16>,
) -> Vec<&'a ServiceRecord> {
    services
        .iter()
        .filter(|service| filters.all || service.state.is_listening())
        .filter(|service| port.is_none_or(|port| service.local.port == port))
        .filter(|service| filters.matches_service(service))
        .collect()
}

/// Flatten and filter connection rows. Without `--all`, terminal/non-live states are omitted.
pub fn filter_connections<'a>(
    services: &'a [ServiceRecord],
    filters: &Filters,
) -> Vec<&'a ConnectionRecord> {
    services
        .iter()
        .flat_map(|service| service.connections.iter())
        .filter(|connection| filters.all || connection.is_active())
        .filter(|connection| filters.matches_connection(connection))
        .collect()
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    let output =
        serde_json::to_string_pretty(value).map_err(|error| CliError::Io(anyhow!(error)))?;
    println!("{output}");
    Ok(())
}

fn print_service_table(rows: &[&ServiceRecord]) {
    let data = rows
        .iter()
        .map(|service| {
            vec![
                service.local.port.to_string(),
                service.protocol.to_string(),
                service.local.to_string(),
                service.state.to_string(),
                service.scope.label().to_string(),
                service.process.name.clone(),
                service.process.pid.to_string(),
                service.process.user.clone().unwrap_or_else(|| "-".into()),
                service.connections.len().to_string(),
            ]
        })
        .collect::<Vec<_>>();
    print!(
        "{}",
        render_table(&SERVICE_HEADERS, &data, terminal_width())
    );
}

fn print_connection_table(rows: &[&ConnectionRecord]) {
    let data = rows
        .iter()
        .map(|connection| {
            vec![
                connection.protocol.to_string(),
                connection.local.to_string(),
                connection.remote.to_string(),
                connection.state.to_string(),
                connection.scope.label().to_string(),
                connection.process.name.clone(),
                connection.process.pid.to_string(),
                connection
                    .process
                    .user
                    .clone()
                    .unwrap_or_else(|| "-".into()),
            ]
        })
        .collect::<Vec<_>>();
    print!(
        "{}",
        render_table(&CONNECTION_HEADERS, &data, terminal_width())
    );
}

fn print_service_details(rows: &[&ServiceRecord], heading: String) {
    println!("{heading}");
    for (index, service) in rows.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("{} {}  {}", service.protocol, service.local, service.state);
        println!("  Scope:   {}", service.scope.label());
        println!("  Service: {}", service.service.as_deref().unwrap_or("-"));
        println!("  Owner:   {}", service.process);
        println!(
            "  User:    {}",
            service.process.user.as_deref().unwrap_or("-")
        );
        println!(
            "  Command: {}",
            service.process.command.as_deref().unwrap_or("-")
        );
        println!(
            "  CWD:     {}",
            service
                .process
                .cwd
                .as_deref()
                .map_or_else(|| "-".into(), |cwd| cwd.display().to_string())
        );
        if service.connections.is_empty() {
            println!("  Connections: none");
        } else {
            println!("  Connections:");
            for connection in &service.connections {
                println!(
                    "    {} -> {}  {}  {}",
                    connection.local, connection.remote, connection.state, connection.process
                );
            }
        }
    }
}

fn print_kill_context(target: &KillTarget, force: bool) {
    let action = if force { "force-kill" } else { "terminate" };
    println!("Will {action} {}", target.process);
    for service in &target.services {
        println!(
            "  {} {} ({})",
            service.protocol,
            service.local,
            service.scope.label()
        );
    }
}

/// Render stable columns with a deterministic width-aware truncation policy.
///
/// The first, protocol, state, PID, and count columns are kept readable before longer text
/// columns are shortened. Missing values should be passed as `-` by callers.
pub fn render_table(headers: &[&str], rows: &[Vec<String>], width: usize) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let mut widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| row.get(index).map_or(0, String::len))
                .fold(header.len(), usize::max)
        })
        .collect::<Vec<_>>();
    let gap = if width <= headers.len().saturating_mul(6) {
        1
    } else {
        2
    };
    let separators = headers.len().saturating_sub(1) * gap;
    while widths.iter().sum::<usize>() + separators > width {
        let essential_candidate = widths
            .iter()
            .enumerate()
            .filter(|(index, value)| **value > headers[*index].len())
            .max_by_key(|(_, value)| **value)
            .map(|(index, _)| index);
        let candidate = essential_candidate.or_else(|| {
            widths
                .iter()
                .enumerate()
                .filter(|(_, value)| **value > 1)
                .max_by_key(|(_, value)| **value)
                .map(|(index, _)| index)
        });
        let Some(index) = candidate else { break };
        widths[index] -= 1;
    }

    let mut output = String::new();
    output.push_str(&format_table_row(
        headers.iter().map(|header| (*header).to_string()).collect(),
        &widths,
        gap,
    ));
    output.push('\n');
    output.push_str(&format_table_row(
        widths.iter().map(|width| "-".repeat(*width)).collect(),
        &widths,
        gap,
    ));
    output.push('\n');
    for row in rows {
        output.push_str(&format_table_row(row.clone(), &widths, gap));
        output.push('\n');
    }
    output
}

fn format_table_row(mut values: Vec<String>, widths: &[usize], gap: usize) -> String {
    values.resize(widths.len(), "-".into());
    let separator = " ".repeat(gap);
    values
        .into_iter()
        .zip(widths.iter().copied())
        .map(|(value, width)| fit_cell(&value, width))
        .collect::<Vec<_>>()
        .join(&separator)
}

fn fit_cell(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let character_count = value.chars().count();
    if character_count <= width {
        return format!("{value:<width$}");
    }
    if width <= 1 {
        return "…".into();
    }
    let mut shortened = value.chars().take(width - 1).collect::<String>();
    shortened.push('…');
    shortened
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width >= 20)
        .unwrap_or(100)
}

const SERVICE_HEADERS: [&str; 9] = [
    "PORT", "PROTO", "ADDRESS", "STATE", "SCOPE", "PROCESS", "PID", "USER", "CONNS",
];
const CONNECTION_HEADERS: [&str; 8] = [
    "PROTO", "LOCAL", "REMOTE", "STATE", "SCOPE", "PROCESS", "PID", "USER",
];

/// Ask for an explicit confirmation, isolated from terminal discovery for deterministic tests.
pub fn confirm_kill<R: std::io::BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    target: &KillTarget,
    force: bool,
) -> Result<bool, CliError> {
    if force {
        write!(writer, "Type KILL to force-kill {}: ", target.process)
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        writer
            .flush()
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        let mut answer = String::new();
        reader
            .read_line(&mut answer)
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        Ok(answer.trim() == "KILL")
    } else {
        write!(writer, "Terminate {}? [y/N] ", target.process)
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        writer
            .flush()
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        let mut answer = String::new();
        reader
            .read_line(&mut answer)
            .map_err(|error| CliError::Io(anyhow!(error)))?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}
