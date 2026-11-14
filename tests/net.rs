//! e2e tests over real loopback sockets

use core::panic;
use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

use blackboard::{
    app::{App, Role},
    net::{Link, NetEvent},
    protocol::{self, Msg},
    relay::{self, RelayConfig},
};
use tungstenite::Message;

const WAIT: Duration = Duration::from_secs(3);

fn start_relay(cfg: RelayConfig) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || relay::serve(listener, cfg));
    format!("ws://{addr}")
}

/// Next message event, skipping nothing; panics on timeout
fn next(link: &Link) -> NetEvent {
    link.recv_timeout(WAIT).expect("timed out waiting for an event")
}

fn expect_msg(link: &Link) -> Msg {
    match next(link) {
        NetEvent::Msg(m) => m,
        other => panic!("expected a message, got {other:?}"),
    }
}

fn room_code(link: &Link) -> String {
    match expect_msg(link) {
        Msg::Room { code } => code,
        other => panic!("expected a room, got {other:?}"),
    }
}

#[test]
fn direct_mode_syncs_board_and_accepts_next_guest() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let host_link = Link::host_direct(listener);
    let mut host = App::new(Role::Host, "host");
    host.board.paint(5, &[[7, 7]]);

    let guest_link = Link::join_direct(&addr).unwrap();
    let mut guest = App::new(Role::Guest, "guest");

    // Pump both sides until the guest has the host's board
    let start = Instant::now();
    while guest.board != host.board || guest.peer_name.is_none() || host.peer_name.is_none() {
        assert!(start.elapsed() < WAIT, "never synced");
        for (app, link) in [(&mut host, &host_link), (&mut guest, &guest_link)] {
            while let Some(ev) = link.try_recv() {
                app.on_net(ev);
            }
            for m in app.take_outbox() {
                link.send(&m);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(guest.peer_name.as_deref(), Some("host"));

    // A second guest is refused while the first one is in
    let intruder = Link::join_direct(&addr).unwrap();
    assert_eq!(expect_msg(&intruder), Msg::PeerJoined);
    assert!(matches!(expect_msg(&intruder), Msg::Error { .. }));
    assert_eq!(expect_msg(&intruder), Msg::PeerLeft);

    // After the first guest leaves, the board takes a new one
    drop(guest_link);
    let start = Instant::now();
    loop {
        if let Some(NetEvent::Msg(Msg::PeerLeft)) = host_link.recv_timeout(WAIT) {
            break;
        }
        assert!(start.elapsed() < WAIT);
    }
    let again = Link::join_direct(&addr).unwrap();
    assert_eq!(expect_msg(&again), Msg::PeerJoined);
    assert_eq!(expect_msg(&host_link), Msg::PeerJoined);
}

#[test]
fn direct_mode_pushes_back_on_a_flooding_guest() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let host = Link::host_direct(listener);
    let mut flood = TcpStream::connect(addr).unwrap();
    flood.set_write_timeout(Some(Duration::from_millis(200))).unwrap();

    // Nobody drains `host`, so when the UI falls behind: the guest must end up blocked on its socket instead of filling
    // the host's memory
    let batch = b"{\"t\":\"clear\"}\n".repeat(1000);
    let start = Instant::now();
    loop {
        match flood.write_all(&batch) {
            Ok(()) => assert!(start.elapsed() < Duration::from_secs(5), "host keeps buffering"),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            Err(e) => panic!("unexpected write error: {e}"),
        }
    }
    drop(host);
}

#[test]
fn relay_pairs_and_forwards_both_ways() {
    let relay = start_relay(RelayConfig::default());
    let host = Link::relay_host(&relay).unwrap();
    let code = room_code(&host);
    let guest = Link::relay_join(&relay, &code.to_lowercase()).unwrap();

    assert_eq!(expect_msg(&host), Msg::PeerJoined);
    assert_eq!(expect_msg(&guest), Msg::PeerJoined);

    assert!(host.send(&Msg::Chat { text: "hi".into() }));
    assert_eq!(expect_msg(&guest), Msg::Chat { text: "hi".into() });
    assert!(guest.send(&Msg::Draw {
        color: 2,
        points: vec![[1, 1]]
    }));
    assert_eq!(
        expect_msg(&host),
        Msg::Draw {
            color: 2,
            points: vec![[1, 1]]
        }
    );

    // Control messages from a peer never reach the other side
    assert!(guest.send(&Msg::PeerLeft));
    assert!(guest.send(&Msg::Clear));
    assert_eq!(expect_msg(&host), Msg::Clear);

    drop(guest);
    assert_eq!(expect_msg(&host), Msg::PeerLeft);
    assert!(matches!(next(&host), NetEvent::Closed(_)));
}

#[test]
fn relay_rejects_unknown_codes_and_codes_are_single_use() {
    let relay = start_relay(RelayConfig::default());
    let guest = Link::relay_join(&relay, "ZZZZZZ").unwrap();
    assert!(matches!(expect_msg(&guest), Msg::Error { .. }));
    assert!(matches!(next(&guest), NetEvent::Closed(_)));

    let host = Link::relay_host(&relay).unwrap();
    let code = room_code(&host);
    let first = Link::relay_join(&relay, &code).unwrap();
    assert_eq!(expect_msg(&first), Msg::PeerJoined);
    let second = Link::relay_join(&relay, &code).unwrap();
    assert!(matches!(expect_msg(&second), Msg::Error { .. }));
}

#[test]
fn relay_refuses_bad_handshakes() {
    let relay = start_relay(RelayConfig::default());
    let (mut ws, _) = tungstenite::connect(&relay).unwrap();
    ws.send(Message::text(protocol::encode(&Msg::Chat { text: "hi".into() })))
        .unwrap();
    let reply = ws.read().unwrap();
    assert!(matches!(
        protocol::decode(reply.to_text().unwrap()),
        Ok(Msg::Error { .. })
    ));

    // Something that is not a WebSocket at all is just dropped
    let addr = relay.trim_start_matches("ws://");
    let mut raw = TcpStream::connect(addr).unwrap();
    raw.set_read_timeout(Some(WAIT)).unwrap();
    raw.write_all(b"{\"t\":\"create\"}\n\n").unwrap();
    let mut reply = Vec::new();
    let _ = raw.read_to_end(&mut reply);
    assert!(!String::from_utf8_lossy(&reply).contains("room"));
}

#[test]
fn relay_refuses_traffic_before_the_guest_joins() {
    let relay = start_relay(RelayConfig::default());
    let host = Link::relay_host(&relay).unwrap();
    room_code(&host);
    assert!(host.send(&Msg::Clear));
    assert!(matches!(expect_msg(&host), Msg::Error { .. }));
    assert!(matches!(next(&host), NetEvent::Closed(_)));
}

#[test]
fn relay_rooms_expire() {
    let relay = start_relay(RelayConfig {
        room_ttl: Duration::from_millis(200),
        ..RelayConfig::default()
    });
    let host = Link::relay_host(&relay).unwrap();
    let code = room_code(&host);
    match expect_msg(&host) {
        Msg::Error { reason } => assert!(reason.contains("nobody joined")),
        other => panic!("expected the room to expire, got {other:?}"),
    }
    let late = Link::relay_join(&relay, &code).unwrap();
    assert!(matches!(expect_msg(&late), Msg::Error { .. }));
}

#[test]
fn relay_counts_clients_behind_a_trusted_proxy_apart() {
    let relay = start_relay(RelayConfig {
        max_conns_per_ip: 1,
        trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
        ..RelayConfig::default()
    });
    let open = |client: &str| {
        let mut req = tungstenite::client::InfoClientRequest::into_client_request(relay.as_str()).unwrap();
        req.headers_mut().insert("x-forwarded-for", client.parse().unwrap());
        let (mut ws, _) = tungstenite::connect(req).unwrap();
        ws.send(Message::text(protocol::encode(&Msg::Create))).unwrap();
        let reply = ws.read().unwrap();
        (ws, protocol::decode(reply.to_text().unwrap()).unwrap());
    };
    let (_a, first) = open("198.51.100.1");
    let (_b, second) = open("198.51.100.2");
    let (_c, third) = open("198.51.100.1");
    assert!(matches!(first, Msg::Room { .. }));
    assert!(matches!(second, Msg::Room { .. }));
    assert!(matches!(third, Msg::Error { .. }));
}

#[test]
fn relay_limits_connections_per_address() {
    let relay = start_relay(RelayConfig {
        max_conns_per_ip: 1,
        ..RelayConfig::default()
    });
    let first = Link::relay_host(&relay).unwrap();
    room_code(&first);
    let second = Link::relay_host(&relay).unwrap();
    match expect_msg(&second) {
        Msg::Error { reason } => assert!(reason.contains("too many")),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // When the first one leaves its slot is free again
    drop(first);
    let start = Instant::now();
    loop {
        let again = Link::relay_host(&relay).unwrap();
        if matches!(expect_msg(&again), Msg::Room { .. }) {
            break;
        }
        assert!(start.elapsed() < WAIT, "the slot was never released");
        thread::sleep(Duration::from_millis(50));
    }
}
