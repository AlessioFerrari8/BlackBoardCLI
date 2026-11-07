//! e2e tests over real loopback sockets

use core::panic;
use std::{io::{ErrorKind, Write}, net::{TcpListener, TcpStream}, thread, time::{Duration, Instant}};

use blackboard::{app::{App, Role}, net::{Link, NetEvent}, protocol::Msg};

const WAIT: Duration = Duration::from_secs(3);

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
            Err(e) => panic!("unexpected write error: {e}")
        }
    }
    drop(host);
}