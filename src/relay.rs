//! Relay server: pairs two clients by room code and forwards their messages over WebSocket
//!
//! The host sends `Create` and gets back `Room { code }`; the guest sends `Join { code }`. Both then receive `PeerJoined` and
//! from that point every peer message one sends is passed to the other. When either side leaves
//! the other gets `PeerLeft` and both connections are closed.
//!
//! The relay speaks plain `ws://`. In front of it a web server (Caddy, the Hack Club Nest ingress, ...) adds TLS, so clients connect to a `wss://` address that
//! passes school and office firewalls.
//!
//! Each connection has one thread that owns its socket. Threads never touch each other's sockets: they talk through channels,
//! and each one checks its channel between short reads.

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use tungstenite::{
    Message, WebSocket,
    handshake::server::{Request, Response},
    protocol::WebSocketConfig,
};

use crate::protocol::{self, MAX_LINE, Msg};

const CODE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 6;
/// How long a read waits before the thread looks at its channel. It is the most delay the relay adds to a message
pub const POLL: Duration = Duration::from_millis(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Messages waiting for the other side. A client that fills it is sending far faster than anyone draws
const QUEUE: usize = 1024;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub max_rooms: usize,
    pub max_conns_per_ip: usize,
    // Time a client has to open the WebSocket and say what it wants
    pub handshake_timeout: Duration,
    // Time a room may wait for its guest
    pub room_ttl: Duration,
    // Proxies in front of the relay (the web server adding TLS). For connections from them the client's address is read
    // from `X-Forwarded-For`, otherwise every client would count as the proxy against `max_conns_per_ip`
    pub trusted_proxies: Vec<IpAddr>,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_rooms: 1000,
            max_conns_per_ip: 8,
            handshake_timeout: Duration::from_secs(10),
            room_ttl: Duration::from_secs(15 * 60),
            trusted_proxies: Vec::new(),
        }
    }
}

/// What one connection's thread tells the other's
enum Ctl {
    /// A guest took the room: this is how to reach it
    Paired(SyncSender<Ctl>),
    Msg(Msg),
    Left,
}

struct State {
    cfg: RelayConfig,
    /// Open rooms, each with the channel of the host waiting in it
    rooms: Mutex<HashMap<String, SyncSender<Ctl>>>,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
}

// Decrements the per-IP counter when the connection ends
struct IpSlot {
    state: Arc<State>,
    ip: IpAddr,
}

impl IpSlot {
    fn take(state: &Arc<State>, ip: IpAddr) -> Option<Self> {
        let mut map = lock(&state.per_ip);
        let n = map.entry(ip).or_insert(0);
        if *n >= state.cfg.max_conns_per_ip {
            return None;
        }
        *n += 1;
        Some(Self {
            state: Arc::clone(state),
            ip,
        })
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut map = lock(&self.state.per_ip);
        if let Some(n) = map.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

type Ws = WebSocket<TcpStream>;

/// WebSocket limits shared by the relay and the client: one message is at most one protocol line
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_LINE))
        .max_frame_size(Some(MAX_LINE))
}

/// Serves forever on `listener`. Logs one line per event to stderr
pub fn serve(listener: TcpListener, cfg: RelayConfig) -> io::Result<()> {
    let state = Arc::new(State {
        cfg,
        rooms: Mutex::new(HashMap::new()),
        per_ip: Mutex::new(HashMap::new()),
    });

    eprintln!("relay listening on ws://{}", listener.local_addr()?);

    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        let Ok(peer) = stream.peer_addr() else { continue };
        let state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(e) = handle(stream, &state) {
                eprintln!("{peer}: {e}")
            }
        });
    }
    Ok(())
}

fn handle(stream: TcpStream, state: &Arc<State>) -> Result<(), String> {
    let peer = stream.peer_addr().map_err(|e| e.to_string())?.ip();
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(state.cfg.handshake_timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut forwarded = None;
    // The error type is tungstenite's, we never return it
    #[allow(clippy::result_large_err)]
    let read_headers = |req: &Request, resp: Response| {
        forwarded = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(resp)
    };
    let mut ws = tungstenite::accept_hdr_with_config(stream, read_headers, Some(ws_config()))
        .map_err(|e| format!("handshake: {e}"))?;
    let ip = client_ip(peer, forwarded.as_deref(), &state.cfg.trusted_proxies);

    let Some(slot) = IpSlot::take(state, ip) else {
        // With a proxy in front, the proxy's address here means --trust-proxy is missing or wrong
        eprintln!("refused {ip} (via {peer}): too many connections");
        refuse(&mut ws, "too many connections from your address");
        return Ok(());
    };
    let first = match read_one(&mut ws) {
        Ok(m) => m,
        Err(e) => {
            refuse(&mut ws, "expected create or join");
            return Err(e);
        }
    };
    ws.get_ref().set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    match first {
        Msg::Create => host(ws, state),
        Msg::Join { code } => guest(ws, state, &code),
        _ => refuse(&mut ws, "expected create or join"),
    }
    drop(slot);
    Ok(())
}

/// Opens a room and stays in it: first waiting for a guest, then paired with it
fn host(mut ws: Ws, state: &State) {
    let (tx, inbox) = mpsc::sync_channel(QUEUE);
    let code = {
        let mut rooms = lock(&state.rooms);
        if rooms.len() >= state.cfg.max_rooms {
            drop(rooms);
            refuse(&mut ws, "the relay is full, try again later");
            return;
        }
        let code = loop {
            let c = new_code();
            if !rooms.contains_key(&c) {
                break c;
            }
        };
        rooms.insert(code.clone(), tx);
        code
    };
    eprintln!("room {code} created");
    if send(&mut ws, &Msg::Room { code: code.clone() }).is_ok() {
        session(&mut ws, &inbox, None, Some(&code), state);
    }

    // Still listed only if nobody joined: a guest removes the room when it takes it
    if lock(&state.rooms).remove(&code).is_some() {
        eprintln!("room {code} abandoned");
    }
    let _ = ws.close(None);
    let _ = ws.flush();
}

fn guest(mut ws: Ws, state: &State, code: &str) {
    let code = code.trim().to_ascii_uppercase();
    let Some(host) = lock(&state.rooms).remove(&code) else {
        refuse(&mut ws, "no room with that code (it may have expired)");
        return;
    };
    let (tx, inbox) = mpsc::sync_channel(QUEUE);
    if host.send(Ctl::Paired(tx)).is_err() {
        refuse(&mut ws, "the host just left");
        return;
    }
    eprintln!("room {code} paired");
    if send(&mut ws, &Msg::PeerJoined).is_ok() {
        session(&mut ws, &inbox, Some(host), None, state);
    }
    eprintln!("room {code} closed");
    let _ = ws.close(None);
    let _ = ws.flush();
}

/// Runs one side of a room until it ends. `waiting` is the room code while the host has no guest yet
fn session(
    ws: &mut Ws,
    inbox: &Receiver<Ctl>,
    mut peer: Option<SyncSender<Ctl>>,
    waiting: Option<&str>,
    state: &State,
) {
    let mut waiting = waiting;
    let opened = Instant::now();
    'session: loop {
        // What our client sent
        match ws.read() {
            Ok(Message::Text(text)) => {
                let Ok(msg) = protocol::decode(text.as_str()) else {
                    refuse(ws, "that is not a blackboard message");
                    break;
                };
                // A peer can never fake a control message
                if !msg.is_peer_msg() {
                    continue;
                }
                let Some(p) = &peer else {
                    refuse(ws, "send nothing before your guest joins");
                    break;
                };
                match p.try_send(Ctl::Msg(msg)) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        refuse(ws, "you are sending too fast");
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Ok(Message::Binary(_)) => {
                refuse(ws, "that is not a blackboard message");
                break;
            }
            Ok(Message::Close(_)) => break,
            // Pings are answered by tungstenite on the next read or write
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if would_block(&e) => {}
            Err(_) => break,
        }

        // What the other side sent us
        loop {
            match inbox.try_recv() {
                Ok(Ctl::Paired(tx)) => {
                    peer = Some(tx);
                    waiting = None;
                    if send(ws, &Msg::PeerJoined).is_err() {
                        break 'session;
                    }
                }
                Ok(Ctl::Msg(m)) => {
                    if send(ws, &m).is_err() {
                        break 'session;
                    }
                }
                Ok(Ctl::Left) => {
                    let _ = send(ws, &Msg::PeerLeft);
                    // The peer is gone: nothing to tell it on the way out
                    peer = None;
                    break 'session;
                }
                Err(TryRecvError::Empty) => break,
                // The host's channel died with a guest's `Paired` still in it, or the other thread ended without a word
                Err(TryRecvError::Disconnected) => {
                    if peer.is_some() {
                        let _ = send(ws, &Msg::PeerLeft);
                    }
                    peer = None;
                    break 'session;
                }
            }
        }
        if let Some(code) = waiting
            && opened.elapsed() > state.cfg.room_ttl
        {
            // If the room is gone a guest has just taken it, and its `Paired` is on the way
            if lock(&state.rooms).remove(code).is_some() {
                eprintln!("room {code} expired");
                refuse(ws, "nobody joined in time, room closed");
                break;
            }
        }
    }
    if let Some(p) = peer {
        let _ = p.send(Ctl::Left);
    }
}

/// The address a connection counts against. Behind a trusted proxy it is the last hop the proxy added to `X-Forwarded-For`:
/// the earlier ones come from the client and could be anything
pub fn client_ip(peer: IpAddr, forwarded: Option<&str>, trusted: &[IpAddr]) -> IpAddr {
    if !trusted.contains(&peer) {
        return peer;
    }
    forwarded
        .and_then(|f| f.rsplit(',').next())
        .and_then(|ip| ip.trim().parse().ok())
        .unwrap_or(peer)
}

/// Reads the first message, waiting up to the handshake timeout set on the socket
fn read_one(ws: &mut Ws) -> Result<Msg, String> {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => return protocol::decode(text.as_str()).map_err(|e| e.to_string()),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(_) => return Err("expected a text message".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn send(ws: &mut Ws, msg: &Msg) -> tungstenite::Result<()> {
    ws.send(Message::text(protocol::encode(msg)))
}

fn refuse(ws: &mut Ws, reason: &str) {
    let _ = send(ws, &Msg::Error { reason: reason.into() });
    let _ = ws.close(None);
    let _ = ws.flush();
}

/// A read that ran into the socket timeout. Unix says `WouldBlock`, Windows `TimedOut`
pub fn would_block(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// True when `s` looks like a room code (any case), not like a host name or address
pub fn is_room_code(s: &str) -> bool {
    let s = s.trim();
    s.len() == CODE_LEN && s.bytes().all(|b| CODE_ALPHABET.contains(&b.to_ascii_uppercase()))
}

fn new_code() -> String {
    (0..CODE_LEN)
        .map(|_| CODE_ALPHABET[rand::random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_use_the_unambiguos_alphabet() {
        for _ in 0..200 {
            let c = new_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)));
            assert!(is_room_code(&c) && is_room_code(&c.to_lowercase()));
        }
        assert!(!is_room_code("192.168.1.2"));
        assert!(!is_room_code("ABC12"), "1 is not in the alphabet");
        assert!(!is_room_code("host:7878"));
    }

    #[test]
    fn forwarded_address_is_trusted_only_from_a_proxy() {
        let proxy: IpAddr = "10.60.1.2".parse().unwrap();
        let other: IpAddr = "203.0.113.9".parse().unwrap();
        let header = Some("1.1.1.1, 192.51.100.7");
        assert_eq!(
            client_ip(proxy, header, &[proxy]),
            "192.51.100.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(client_ip(other, header, &[proxy]), other, "anyone can write the header");
        assert_eq!(client_ip(proxy, None, &[proxy]), proxy);
        assert_eq!(client_ip(proxy, Some("garbage"), &[proxy]), proxy);
    }
}
