//! macOS socket discovery backed by `/usr/sbin/lsof`.
//!
//! The parser deliberately accepts a little more than the exact output emitted by
//! our command.  lsof can omit fields for processes hidden by permissions, and
//! older releases use both `ST=` and `TST=` TCP state keys.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Context, Result};

use crate::model::{
    ConnectionRecord, Endpoint, ProcessMetadata, Protocol, ServiceRecord, SocketRecord, SocketState,
};

const LSOF: &str = "/usr/sbin/lsof";

/// Discover sockets and group them into listener/service rows.
///
/// The lsof invocation itself is the only required command. Process command
/// lines are filled in with one `ps` call, while working directories and
/// executable paths come from one bounded, multi-PID lsof lookup; failures in
/// either enrichment pass leave the information absent rather than dropping an
/// otherwise useful socket.
pub fn discover() -> Result<Vec<ServiceRecord>> {
    let output = Command::new(LSOF)
        .args(["-nP", "-F0pcuLafntPT", "-iTCP", "-iUDP"])
        .output()
        .with_context(|| format!("failed to execute {LSOF}"))?;

    let mut sockets = parse_lsof(&output.stdout);
    enrich_processes(&mut sockets);
    Ok(services_from_sockets(sockets))
}

/// Send SIGTERM, or SIGKILL when `force` is true, to a validated process ID.
///
/// PID zero, PID one, the current process, and values outside the platform's
/// signed PID range are rejected before touching the kernel.  This keeps the
/// operation safe for ordinary users and produces a useful error for a stale
/// or inaccessible process.
pub fn terminate_pid(pid: u32, force: bool) -> Result<()> {
    if pid <= 1 {
        return Err(anyhow!("refusing to terminate unsafe pid {pid}"));
    }
    if pid > i32::MAX as u32 {
        return Err(anyhow!("pid {pid} is outside the macOS pid range"));
    }
    if pid == std::process::id() {
        return Err(anyhow!(
            "refusing to terminate the current process (pid {pid})"
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: pid and signal have been validated, and kill does not retain
        // any pointers or references beyond this call.
        let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
        if result == -1 {
            return Err(anyhow!(
                "failed to send {} to pid {pid}: {}",
                if force { "SIGKILL" } else { "SIGTERM" },
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = force;
        Err(anyhow!("socket termination is only supported on macOS"))
    }
}

/// Parse NUL- or newline-terminated lsof field output into independent sockets.
///
/// This is public so fixtures and downstream integrations can exercise the
/// platform parser without invoking lsof.  Malformed process/file records are
/// ignored individually; valid records in the same stream are retained.
pub fn parse_lsof(input: &[u8]) -> Vec<SocketRecord> {
    let mut sockets = Vec::new();
    let mut process: Option<RawProcess> = None;
    let mut file: Option<RawFile> = None;

    for token in input.split(|byte| *byte == 0 || *byte == b'\n' || *byte == b'\r') {
        if token.is_empty() {
            continue;
        }
        let key = token[0] as char;
        let value = String::from_utf8_lossy(&token[1..]).trim().to_owned();
        match key {
            'p' => {
                flush_file(&mut sockets, process.as_ref(), file.take());
                process = Some(RawProcess::from_pid(&value));
            }
            'c' => {
                if let Some(current) = process.as_mut() {
                    current.name = Some(value);
                }
            }
            'u' => {
                if let Some(current) = process.as_mut() {
                    current.uid = value.parse().ok();
                }
            }
            'L' => {
                if let Some(current) = process.as_mut() {
                    if !value.is_empty() && value != "(unknown)" && value != "?" {
                        current.user = Some(value);
                    }
                }
            }
            'f' => {
                flush_file(&mut sockets, process.as_ref(), file.take());
                file = Some(RawFile::default());
            }
            'P' => {
                if let Some(current) = file.as_mut() {
                    current.protocol = match value.to_ascii_uppercase().as_str() {
                        "TCP" => Some(Protocol::Tcp),
                        "UDP" => Some(Protocol::Udp),
                        _ => None,
                    };
                }
            }
            't' => {
                if let Some(current) = file.as_mut() {
                    current.family = match value.to_ascii_lowercase().as_str() {
                        "ipv4" => AddressFamily::V4,
                        "ipv6" => AddressFamily::V6,
                        _ => AddressFamily::Unknown,
                    };
                }
            }
            'n' => {
                if let Some(current) = file.as_mut() {
                    current.name = Some(value);
                }
            }
            'T' => {
                if let Some(current) = file.as_mut() {
                    if let Some(state) = parse_state_hint(&value) {
                        current.state = Some(state);
                    }
                }
            }
            _ => {}
        }
    }
    flush_file(&mut sockets, process.as_ref(), file.take());

    sockets.sort();
    sockets.dedup();
    sockets
}

/// Group parsed sockets into deterministic service rows.
///
/// Listening sockets owned by one process and sharing a protocol/port are
/// represented by one service, even when the kernel reports separate IPv4 and
/// IPv6 bindings. Every binding is retained, while the primary `local`
/// endpoint remains deterministic. Non-listening records retain their
/// address/state grouping so unrelated socket rows are not merged. A
/// peer-bearing socket is attached to the best listener owned by the same
/// process (exact local address wins over an address-family wildcard). When no
/// listener is visible, the socket remains a standalone service row with its
/// connection retained.
pub fn services_from_sockets(mut sockets: Vec<SocketRecord>) -> Vec<ServiceRecord> {
    sockets.sort();
    sockets.dedup();

    let mut services = Vec::new();
    for socket in sockets.iter().filter(|socket| socket.remote.is_none()) {
        let matching = services
            .iter()
            .position(|service: &ServiceRecord| service_group_matches(service, socket));
        if let Some(index) = matching {
            services[index].add_binding(socket.local.clone());
        } else {
            services.push(ServiceRecord::new(
                socket.protocol,
                socket.local.clone(),
                socket.state.clone(),
                socket.process.clone(),
                None,
            ));
        }
    }

    let connections = sockets
        .into_iter()
        .filter_map(|socket| {
            let remote = socket.remote.clone()?;
            if socket.state.is_terminal() {
                return None;
            }
            Some(ConnectionRecord::new(
                socket.protocol,
                socket.local,
                remote,
                socket.state,
                socket.process,
            ))
        })
        .collect::<Vec<_>>();

    for connection in connections {
        let matching = services
            .iter()
            .enumerate()
            .filter_map(|(index, service)| {
                listener_match_score(service, &connection).map(|score| (score, index))
            })
            .max_by_key(|(score, index)| (*score, std::cmp::Reverse(*index)));

        if let Some((_, index)) = matching {
            services[index].add_connection(connection);
        } else {
            let mut service = ServiceRecord::new(
                connection.protocol,
                connection.local.clone(),
                connection.state.clone(),
                connection.process.clone(),
                None,
            );
            service.add_connection(connection);
            services.push(service);
        }
    }

    for service in &mut services {
        service.connections.sort();
        service.connections.dedup();
    }
    services.sort();
    services.dedup();
    services
}

#[derive(Clone, Copy, Debug, Default)]
enum AddressFamily {
    V4,
    V6,
    #[default]
    Unknown,
}

#[derive(Default)]
struct RawProcess {
    pid: Option<u32>,
    name: Option<String>,
    uid: Option<u32>,
    user: Option<String>,
}

impl RawProcess {
    fn from_pid(value: &str) -> Self {
        Self {
            pid: value.parse().ok().filter(|pid| *pid > 0),
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct RawFile {
    protocol: Option<Protocol>,
    family: AddressFamily,
    name: Option<String>,
    state: Option<String>,
}

fn flush_file(
    sockets: &mut Vec<SocketRecord>,
    process: Option<&RawProcess>,
    file: Option<RawFile>,
) {
    let Some(process) = process else { return };
    let Some(file) = file else { return };
    let Some(pid) = process.pid else { return };
    let Some(protocol) = file.protocol else {
        return;
    };
    let Some(name) = file.name.as_deref() else {
        return;
    };
    let Some((local, remote)) = parse_socket_name(name, file.family) else {
        return;
    };

    let user = process
        .user
        .clone()
        .or_else(|| process.uid.map(|uid| uid.to_string()));
    let (process_name, executable) =
        process_identity(process.name.as_deref().unwrap_or_default(), None);
    let mut metadata = ProcessMetadata::new(pid, process_name);
    metadata.executable = executable;
    metadata.user = user;
    metadata.is_current_user = is_current_user(metadata.user.as_deref());

    let state = socket_state(file.state.as_deref(), protocol, remote.is_some());
    let socket = SocketRecord::new(protocol, local, state, metadata);
    sockets.push(match remote {
        Some(remote) => socket.with_remote(remote),
        None => socket,
    });
}

fn parse_state_hint(value: &str) -> Option<String> {
    let (key, state) = value.split_once('=')?;
    if key.eq_ignore_ascii_case("st") || key.eq_ignore_ascii_case("tst") {
        let state = state.trim();
        if !state.is_empty() {
            return Some(state.to_owned());
        }
    }
    None
}

fn socket_state(raw: Option<&str>, protocol: Protocol, has_remote: bool) -> SocketState {
    if let Some(raw) = raw {
        let normalized = raw.trim().to_ascii_uppercase();
        return match normalized.as_str() {
            "LISTEN" | "LISTENING" => SocketState::Listening,
            "ESTABLISHED" => SocketState::Established,
            "SYN-SENT" | "SYNSENT" => SocketState::SynSent,
            "SYN-RECEIVED" | "SYNRECV" => SocketState::SynReceived,
            "FIN-WAIT-1" | "FIN_WAIT_1" => SocketState::FinWait1,
            "FIN-WAIT-2" | "FIN_WAIT_2" => SocketState::FinWait2,
            "TIME-WAIT" | "TIME_WAIT" => SocketState::TimeWait,
            "CLOSED" | "CLOSE" => SocketState::Close,
            "CLOSE-WAIT" | "CLOSE_WAIT" => SocketState::CloseWait,
            "LAST-ACK" | "LAST_ACK" => SocketState::LastAck,
            "CLOSING" => SocketState::Closing,
            "BOUND" => SocketState::Bound,
            "UNCONNECTED" | "UNCONN" => SocketState::Unconnected,
            _ => SocketState::Other(raw.to_owned()),
        };
    }

    match (protocol, has_remote) {
        (Protocol::Tcp, false) => SocketState::Listening,
        (Protocol::Udp, false) => SocketState::Bound,
        (_, true) => SocketState::Established,
    }
}

fn parse_socket_name(name: &str, family: AddressFamily) -> Option<(Endpoint, Option<Endpoint>)> {
    let (local_name, remote_name) = name.split_once("->").unwrap_or((name, ""));
    let local = parse_endpoint(local_name, family)?;
    let remote = if remote_name.trim().is_empty() {
        None
    } else {
        parse_endpoint(remote_name, family)
    };
    Some((local, remote))
}

fn parse_endpoint(value: &str, family: AddressFamily) -> Option<Endpoint> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let (host, port) = if value.starts_with('[') {
        let close = value.find(']')?;
        let host = &value[1..close];
        let rest = value.get(close + 1..)?.strip_prefix(':')?;
        (host, rest)
    } else {
        let (host, port) = value.rsplit_once(':')?;
        (host, port)
    };
    let port = port.trim().parse::<u16>().ok()?;
    let host = host.trim();
    let inferred_family = if host.contains(':') {
        AddressFamily::V6
    } else {
        AddressFamily::V4
    };
    let family = if matches!(family, AddressFamily::Unknown) {
        inferred_family
    } else {
        family
    };
    let address = if host == "*" {
        match family {
            AddressFamily::V6 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            _ => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        }
    } else {
        let host = host.split('%').next().unwrap_or(host);
        host.parse::<IpAddr>().ok()?
    };
    Some(Endpoint::new(address, port))
}

fn service_group_matches(service: &ServiceRecord, socket: &SocketRecord) -> bool {
    if service.protocol != socket.protocol
        || service.process.pid != socket.process.pid
        || service.local.port != socket.local.port
    {
        return false;
    }
    if service.state.is_listening() && socket.state.is_listening() {
        return true;
    }
    service.local == socket.local && service.state == socket.state
}

fn listener_match_score(service: &ServiceRecord, connection: &ConnectionRecord) -> Option<u8> {
    if !service.state.is_listening()
        || service.protocol != connection.protocol
        || service.process.pid != connection.process.pid
        || service.local.port != connection.local.port
    {
        return None;
    }

    service
        .bindings
        .iter()
        .filter_map(|binding| {
            if binding.address == connection.local.address {
                Some(2)
            } else if binding.address.is_unspecified()
                && binding.address.is_ipv4() == connection.local.address.is_ipv4()
            {
                Some(1)
            } else {
                None
            }
        })
        .max()
}

fn enrich_processes(sockets: &mut [SocketRecord]) {
    let pids = sockets
        .iter()
        .map(|socket| socket.process.pid)
        .filter(|pid| *pid > 1)
        .collect::<BTreeSet<_>>();
    if pids.is_empty() {
        return;
    }

    let mut metadata = BTreeMap::<u32, ProcessMetadata>::new();
    for socket in sockets.iter() {
        metadata
            .entry(socket.process.pid)
            .or_insert_with(|| socket.process.clone());
    }

    if let Some(ps) = read_ps(&pids) {
        for (pid, enriched) in ps {
            if let Some(current) = metadata.get_mut(&pid) {
                merge_metadata(current, enriched);
            }
        }
    }
    if let Some(paths) = read_process_paths(&pids) {
        for (pid, paths) in paths {
            if let Some(current) = metadata.get_mut(&pid) {
                if let Some(cwd) = paths.cwd {
                    current.cwd = Some(cwd);
                }
                if let Some(executable) = paths.executable {
                    current.name = executable
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| current.name.clone(), str::to_owned);
                    current.executable = Some(executable);
                }
            }
        }
    }

    for socket in sockets {
        if let Some(process) = metadata.get(&socket.process.pid) {
            socket.process = process.clone();
        }
    }
}

fn merge_metadata(current: &mut ProcessMetadata, enriched: ProcessMetadata) {
    if !enriched.name.is_empty() {
        current.name = enriched.name;
    }
    if enriched.executable.is_some() {
        current.executable = enriched.executable;
    }
    if enriched.command.is_some() {
        current.command = enriched.command;
    }
    if enriched.cwd.is_some() {
        current.cwd = enriched.cwd;
    }
    if enriched.user.is_some() {
        current.user = enriched.user;
    }
    current.is_current_user = enriched.is_current_user || current.is_current_user;
}

fn read_ps(pids: &BTreeSet<u32>) -> Option<BTreeMap<u32, ProcessMetadata>> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("/bin/ps")
        .args(["-ww", "-p", &list, "-o", "pid=,user=,comm=,command="])
        .output()
        .ok()?;
    Some(parse_ps(&output.stdout))
}

fn parse_ps(input: &[u8]) -> BTreeMap<u32, ProcessMetadata> {
    let mut result = BTreeMap::new();
    for line in String::from_utf8_lossy(input).lines() {
        let mut fields = line.trim_start().splitn(3, char::is_whitespace);
        let Some(pid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let user = fields
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let rest = fields.next().map(str::trim).unwrap_or_default();
        let (raw_name, command) = split_ps_fields(rest);
        let (name, executable) = process_identity(raw_name, command);
        let mut process = ProcessMetadata::new(pid, name);
        process.executable = executable;
        process.user = user.map(str::to_owned);
        process.command = command.map(str::to_owned);
        process.is_current_user = is_current_user(process.user.as_deref());
        result.insert(pid, process);
    }
    result
}

fn split_ps_fields(rest: &str) -> (&str, Option<&str>) {
    let absolute_starts = rest
        .char_indices()
        .filter_map(|(index, character)| {
            (character == '/'
                && rest[..index]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    match absolute_starts.as_slice() {
        [first, second, ..] if *first == 0 => {
            (rest[..*second].trim(), Some(rest[*second..].trim()))
        }
        [command_start, ..] => (
            rest[..*command_start].trim(),
            Some(rest[*command_start..].trim()),
        ),
        [] => (
            rest.split_whitespace().next().unwrap_or(rest),
            (!rest.is_empty()).then_some(rest),
        ),
    }
}

fn process_identity(raw_name: &str, command: Option<&str>) -> (String, Option<PathBuf>) {
    let executable = if Path::new(raw_name).is_absolute() {
        absolute_executable(raw_name)
    } else {
        command.and_then(|command| absolute_executable_from_command(command, raw_name))
    };
    let name = executable
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(raw_name)
        .to_owned();
    (name, executable)
}

fn absolute_executable_from_command(command: &str, raw_name: &str) -> Option<PathBuf> {
    let command = command.trim();
    if !command.starts_with('/') || raw_name.is_empty() {
        return None;
    }
    let boundaries = command
        .char_indices()
        .filter_map(|(index, character)| character.is_whitespace().then_some(index))
        .chain(std::iter::once(command.len()));
    for end in boundaries {
        let candidate = command[..end].trim_end();
        if candidate.is_empty() {
            continue;
        }
        let path = Path::new(candidate);
        let file_name = path.file_name().and_then(|name| name.to_str());
        if file_name == Some(raw_name) || file_name.is_some_and(|name| name.starts_with(raw_name)) {
            return Some(path.to_path_buf());
        }
    }
    None
}

fn absolute_executable(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    path.is_absolute().then(|| path.to_path_buf())
}

#[derive(Default)]
struct ProcessPaths {
    cwd: Option<PathBuf>,
    executable: Option<PathBuf>,
}

fn read_process_paths(pids: &BTreeSet<u32>) -> Option<BTreeMap<u32, ProcessPaths>> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new(LSOF)
        .args(["-nP", "-a", "-d", "cwd,txt", "-p", &list, "-F0pfn"])
        .output()
        .ok()?;
    Some(parse_process_paths(&output.stdout))
}

fn parse_process_paths(input: &[u8]) -> BTreeMap<u32, ProcessPaths> {
    let mut result = BTreeMap::<u32, ProcessPaths>::new();
    let mut pid = None;
    let mut fd = None;
    for token in input.split(|byte| *byte == 0 || *byte == b'\n' || *byte == b'\r') {
        if token.is_empty() {
            continue;
        }
        let key = token[0] as char;
        let value = String::from_utf8_lossy(&token[1..]).trim().to_owned();
        match key {
            'p' => {
                pid = value.parse::<u32>().ok();
                fd = None;
            }
            'f' => fd = Some(value),
            'n' => {
                let Some(pid) = pid else { continue };
                if value.is_empty() || value == "(unknown)" || value == "?" {
                    continue;
                }
                let paths = result.entry(pid).or_default();
                match fd.as_deref() {
                    Some("cwd") => paths.cwd = Some(PathBuf::from(value)),
                    Some("txt") if paths.executable.is_none() => {
                        paths.executable = Some(PathBuf::from(value));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    result
}

fn current_user_name() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|value| !value.is_empty())
        })
}

fn is_current_user(user: Option<&str>) -> bool {
    let Some(user) = user else {
        return false;
    };
    if current_user_name().as_deref() == Some(user) {
        return true;
    }

    #[cfg(target_os = "macos")]
    {
        user.parse::<libc::uid_t>().ok() == Some(unsafe { libc::geteuid() })
    }

    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_metadata_uses_basename_and_preserves_executable_path() {
        let parsed = parse_ps(
            b"2710 noah /Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted /Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted --flag\n",
        );
        let process = parsed.get(&2710).expect("parsed process");

        assert_eq!(process.name, "remoted");
        assert_eq!(
            process.executable.as_deref(),
            Some(Path::new(
                "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted"
            ))
        );
        assert_eq!(
            process.command.as_deref(),
            Some(
                "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted --flag"
            )
        );
    }

    #[test]
    fn ps_metadata_falls_back_to_absolute_command_path() {
        let parsed = parse_ps(b"42 noah remoted /usr/libexec/remoted --daemon\n");
        let process = parsed.get(&42).expect("parsed process");

        assert_eq!(process.name, "remoted");
        assert_eq!(
            process.executable.as_deref(),
            Some(Path::new("/usr/libexec/remoted"))
        );
    }

    #[test]
    fn ps_metadata_keeps_spaces_in_command_and_resolves_path() {
        let parsed = parse_ps(
            b"70482 noah Google Chrome H /Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper --type=utility\n",
        );
        let process = parsed.get(&70482).expect("parsed process");

        assert_eq!(process.name, "Google Chrome Helper");
        assert_eq!(
            process.executable.as_deref(),
            Some(Path::new(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper"
            ))
        );
        assert_eq!(
            process.command.as_deref(),
            Some(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome Helper --type=utility"
            )
        );
    }

    #[test]
    fn lsof_metadata_normalizes_absolute_process_names() {
        let sockets = parse_lsof(
            b"p2710\0c/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted\0u501\0f3u\0tIPv4\0PTCP\0n*:8080\0TST=LISTEN\0",
        );
        let process = &sockets[0].process;

        assert_eq!(process.name, "remoted");
        assert_eq!(
            process.executable.as_deref(),
            Some(Path::new(
                "/Library/Apple/System/Library/PrivateFrameworks/Remote.framework/Support/remoted"
            ))
        );
    }

    #[test]
    fn lsof_process_paths_keep_cwd_and_first_executable() {
        let paths = parse_process_paths(
            b"p42\0fcwd\0n/Users/alice\0ftxt\0n/usr/bin/server\0ftxt\0n/usr/lib/libhelper.dylib\0pbad\0fcwd\0n?\0",
        );
        let process = paths.get(&42).expect("parsed process paths");

        assert_eq!(process.cwd.as_deref(), Some(Path::new("/Users/alice")));
        assert_eq!(
            process.executable.as_deref(),
            Some(Path::new("/usr/bin/server"))
        );
    }
}
