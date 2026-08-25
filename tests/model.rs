use std::{net::IpAddr, path::PathBuf};

use ports::{
    filter::{Filter, FilterSet},
    model::{
        ConnectionRecord, Endpoint, NetworkScope, ProcessMetadata, Protocol, ServiceRecord,
        SocketState,
    },
};

fn ip(value: &str) -> IpAddr {
    value.parse().expect("valid test address")
}

#[test]
fn classifies_ipv4_scope_boundaries() {
    assert_eq!(
        NetworkScope::classify(ip("0.0.0.0")),
        NetworkScope::AllInterfaces
    );
    assert_eq!(
        NetworkScope::classify(ip("127.255.255.255")),
        NetworkScope::Loopback
    );
    assert_eq!(
        NetworkScope::classify(ip("10.0.0.1")),
        NetworkScope::Private
    );
    assert_eq!(
        NetworkScope::classify(ip("172.31.255.255")),
        NetworkScope::Private
    );
    assert_eq!(
        NetworkScope::classify(ip("192.168.0.1")),
        NetworkScope::Private
    );
    assert_eq!(
        NetworkScope::classify(ip("169.254.0.1")),
        NetworkScope::LinkLocal
    );
    assert_eq!(
        NetworkScope::classify(ip("100.63.255.255")),
        NetworkScope::External
    );
    assert_eq!(
        NetworkScope::classify(ip("100.64.0.0")),
        NetworkScope::Tailscale
    );
    assert_eq!(
        NetworkScope::classify(ip("100.127.255.255")),
        NetworkScope::Tailscale
    );
    assert_eq!(
        NetworkScope::classify(ip("100.128.0.0")),
        NetworkScope::External
    );
    assert_eq!(
        NetworkScope::classify(ip("8.8.8.8")),
        NetworkScope::External
    );
}

#[test]
fn classifies_ipv6_scope_boundaries() {
    assert_eq!(
        NetworkScope::classify(ip("::")),
        NetworkScope::AllInterfaces
    );
    assert_eq!(NetworkScope::classify(ip("::1")), NetworkScope::Loopback);
    assert_eq!(
        NetworkScope::classify(ip("fe80::1")),
        NetworkScope::LinkLocal
    );
    assert_eq!(NetworkScope::classify(ip("fc00::1")), NetworkScope::Private);
    assert_eq!(
        NetworkScope::classify(ip("fd7a:115c:a1e0::1")),
        NetworkScope::Tailscale
    );
    assert_eq!(
        NetworkScope::classify(ip("fd7a:115c:a1e1::1")),
        NetworkScope::Private
    );
    assert_eq!(
        NetworkScope::classify(ip("2001:db8::1")),
        NetworkScope::External
    );
}

fn sample_service() -> ServiceRecord {
    let mut process = ProcessMetadata::new(4242, "web-server");
    process.command = Some("web-server --listen".into());
    process.cwd = Some(PathBuf::from("/Users/alice/projects/ports"));
    process.user = Some("alice".into());
    process.is_current_user = true;

    let local = Endpoint::new(ip("127.0.0.1"), 8080);
    let mut service = ServiceRecord::new(
        Protocol::Tcp,
        local.clone(),
        SocketState::Listening,
        process.clone(),
        Some("web".into()),
    );
    service.add_connection(ConnectionRecord::new(
        Protocol::Tcp,
        local,
        Endpoint::new(ip("100.100.20.30"), 443),
        SocketState::Established,
        process,
    ));
    service
}

#[test]
fn composed_filters_cover_process_endpoint_and_connection_fields() {
    let service = sample_service();
    let services = [service.clone()];

    let filter = Filter::default()
        .with_port(443)
        .with_pid(4242)
        .with_process("WEB")
        .with_protocol(Protocol::Tcp)
        .with_address("100.100.20.30")
        .with_cwd("projects/ports")
        .with_state(SocketState::Established)
        .with_scope(NetworkScope::Loopback)
        .for_current_user()
        .with_active_connection()
        .with_search("web-server 100.100.20.30");

    assert!(filter.matches(&service));
    assert_eq!(filter.apply(&services), vec![&service]);

    let mut set = FilterSet::new();
    set.push(Filter::default().with_protocol(Protocol::Tcp));
    set.push(Filter::default().with_pid(4242));
    assert_eq!(set.apply(&services), vec![&service]);

    assert!(!Filter::default().with_pid(9).matches(&service));
    assert!(!Filter::default()
        .with_state(SocketState::Close)
        .matches(&service));
}

#[test]
fn endpoint_display_preserves_ipv6_and_json_shape() {
    let endpoint = Endpoint::new(ip("2001:db8::1"), 443);
    assert_eq!(endpoint.to_string(), "[2001:db8::1]:443");
    let json = serde_json::to_string(&endpoint).expect("endpoint serializes");
    assert!(json.contains("2001:db8::1"));
    assert!(json.contains("443"));
}
