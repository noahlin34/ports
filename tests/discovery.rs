use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ports::{
    discovery::{parse_lsof, services_from_sockets},
    model::{Protocol, SocketState},
};

#[test]
fn parses_ipv4_ipv6_udp_peers_duplicates_and_permission_gaps() {
    let fixture = b"p100\0cweb\0u501\0Lalice\0f3u\0tIPv4\0PTCP\0n*:8080\0TST=LISTEN\0f3u\0tIPv4\0PTCP\0n*:8080\0TST=LISTEN\0f4u\0tIPv4\0PTCP\0n192.0.2.10:8080->198.51.100.20:443\0TST=ESTABLISHED\0f5u\0tIPv6\0PTCP\0n[::1]:9000->[2001:db8::20]:443\0TST=ESTABLISHED\np101\ncudp-service\nu501\nLalice\nf6u\ntIPv4\nPUDP\nn0.0.0.0:5353\np102\nf7u\ntIPv6\nPUDP\nn[::]:5353\np103\ncpermission-hidden\nf7u\ntIPv4\nPTCP\nn127.0.0.1:1234\nfbad\ntIPv4\nPTCP\nnnot-an-endpoint\npbad\ncbad\nf8u\ntIPv4\nPTCP\nn*:99\n";

    let sockets = parse_lsof(fixture);
    assert_eq!(sockets.len(), 6, "duplicate and malformed rows are ignored");

    let wildcard = sockets
        .iter()
        .find(|socket| socket.local.port == 8080 && socket.remote.is_none())
        .expect("IPv4 wildcard listener");
    assert_eq!(wildcard.protocol, Protocol::Tcp);
    assert_eq!(wildcard.state, SocketState::Listening);
    assert_eq!(wildcard.local.address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(wildcard.process.pid, 100);
    assert_eq!(wildcard.process.name, "web");
    assert_eq!(wildcard.process.user.as_deref(), Some("alice"));

    let established_v4 = sockets
        .iter()
        .find(|socket| socket.remote.as_ref().is_some_and(|peer| peer.port == 443))
        .expect("IPv4 established peer");
    assert_eq!(established_v4.state, SocketState::Established);
    assert_eq!(
        established_v4.local.address,
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))
    );
    assert_eq!(
        established_v4.remote.as_ref().unwrap().address,
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 20))
    );

    let established_v6 = sockets
        .iter()
        .find(|socket| socket.local.port == 9000)
        .expect("bracketed IPv6 peer");
    assert_eq!(
        established_v6.local.address,
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
    assert_eq!(
        established_v6.remote.as_ref().unwrap().address,
        "2001:db8::20".parse::<IpAddr>().unwrap()
    );

    let udp_wildcard = sockets
        .iter()
        .find(|socket| socket.protocol == Protocol::Udp && socket.local.port == 5353)
        .expect("UDP wildcard listener");
    assert_eq!(udp_wildcard.state, SocketState::Bound);
    assert_eq!(udp_wildcard.process.user.as_deref(), Some("alice"));

    let permission_gap = sockets
        .iter()
        .find(|socket| socket.process.pid == 103)
        .expect("record with omitted UID/login");
    assert!(permission_gap.process.user.is_none());
}

#[test]
fn correlates_wildcard_listener_and_keeps_standalone_active_socket() {
    let fixture = b"p42\ncserver\nu501\nLalice\nf3u\ntIPv4\nPTCP\nn*:8080\nTST=LISTEN\nf4u\ntIPv4\nPTCP\nn192.0.2.10:8080->198.51.100.20:443\nTST=ESTABLISHED\np77\ncmobile\nf5u\ntIPv6\nPTCP\nn[::1]:9000->[2001:db8::2]:443\nTST=ESTABLISHED\n";
    let services = services_from_sockets(parse_lsof(fixture));

    let listener = services
        .iter()
        .find(|service| service.process.pid == 42)
        .expect("listener service");
    assert_eq!(listener.local.address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(listener.connections.len(), 1);
    assert_eq!(listener.connections[0].remote.port, 443);

    let standalone = services
        .iter()
        .find(|service| service.process.pid == 77)
        .expect("standalone active socket");
    assert_eq!(standalone.state, SocketState::Established);
    assert_eq!(standalone.connections.len(), 1);
    assert_eq!(
        standalone.connections[0].remote.address,
        "2001:db8::2".parse::<IpAddr>().unwrap()
    );
}
