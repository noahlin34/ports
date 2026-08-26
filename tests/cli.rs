#[allow(dead_code)]
#[path = "../src/cli.rs"]
mod cli;

use clap::Parser;
use ports::model::{Endpoint, NetworkScope, ProcessMetadata, Protocol, ServiceRecord, SocketState};
use std::net::{IpAddr, Ipv4Addr};

fn service(port: u16, pid: u32, state: SocketState, user: Option<&str>) -> ServiceRecord {
    let mut process = ProcessMetadata::new(pid, format!("worker-{pid}"));
    process.user = user.map(str::to_owned);
    process.is_current_user = user == Some("me");
    ServiceRecord::new(
        Protocol::Tcp,
        Endpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        state,
        process,
        None,
    )
}

#[test]
fn parses_commands_and_composed_filters() {
    let cli = cli::Cli::try_parse_from([
        "ports",
        "list",
        "--protocol",
        "tcp",
        "--state",
        "listening",
        "--scope",
        "loopback",
        "--process",
        "web",
        "--pid",
        "42",
        "--user",
        "me",
        "--current-user",
        "--active-connections",
        "--all",
        "--json",
    ])
    .expect("valid CLI");
    let cli::Command::List(args) = cli.command.expect("subcommand") else {
        panic!("expected list")
    };
    assert_eq!(args.filters.protocol, Some(Protocol::Tcp));
    assert_eq!(args.filters.state, Some(SocketState::Listening));
    assert_eq!(args.filters.scope, Some(NetworkScope::Loopback));
    assert_eq!(args.filters.pid, Some(42));
    assert!(args.filters.current_user);
    assert!(args.filters.active_connections);
    assert!(args.filters.all);
    assert!(args.json);
}

#[test]
fn no_subcommand_is_the_tui_mode() {
    let cli = cli::Cli::try_parse_from(["ports"]).expect("valid CLI");
    assert!(cli.command.is_none());
}

#[test]
fn filters_hide_non_listeners_unless_all_is_set() {
    let services = vec![
        service(8080, 7, SocketState::Listening, Some("me")),
        service(8081, 8, SocketState::Established, Some("other")),
    ];
    let filters = cli::Filters::default();
    assert_eq!(cli::filter_services(&services, &filters, None).len(), 1);

    let mut all = filters.clone();
    all.all = true;
    assert_eq!(cli::filter_services(&services, &all, None).len(), 2);

    let mut current = all;
    current.current_user = true;
    assert_eq!(cli::filter_services(&services, &current, None).len(), 1);
}

#[test]
fn narrow_tables_truncate_without_losing_headers() {
    let table = cli::render_table(
        &["PORT", "PROCESS", "STATE"],
        &[vec![
            "8080".into(),
            "very-long-process-name".into(),
            "LISTEN".into(),
        ]],
        18,
    );
    assert!(table.contains("PORT"));
    assert!(table.contains("PROCESS"));
    assert!(table.contains('…'));
    assert!(table.lines().all(|line| line.chars().count() <= 18));
}

#[test]
fn kill_resolution_groups_one_pid_and_rejects_multiple_owners() {
    let services = vec![
        service(8080, 7, SocketState::Listening, Some("me")),
        service(8080, 7, SocketState::Bound, Some("me")),
        service(8080, 8, SocketState::Listening, Some("other")),
    ];
    let targets = cli::resolve_kill_targets(&services, 8080);
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].services.len(), 2);
    let error = cli::CliError::Ambiguous {
        port: 8080,
        owners: targets.into_iter().map(|target| target.process).collect(),
    };
    assert_eq!(error.exit_code(), 2);
}
