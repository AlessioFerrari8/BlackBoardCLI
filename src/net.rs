//! Client-side networking

use std::{
    io::BufReader, net::{Shutdown, TcpListener, TcpStream}, sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    }, thread, time::Duration,
};

use crate::protocol::{self, Msg};
use std::net::ToSocketAddrs;

use anyhow::{Context, Result, anyhow};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

// Events waiting for the UI. When the queue is full the reader stops reading, so a peer sending too fast is held back by TCP
// instead of filling our memory
const EVENT_QUEUE: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent {
    /// A message from the peer, or a pairing event (`PeerJoined`, `PeerLeft`, `Room`, `Error`) from the relay or from the
    /// local listener
    Msg(Msg),
    /// Connection gode: nothing more will arrive
    Closed(String),
}

type Slot = Arc<Mutex<Option<TcpStream>>>;

pub struct Link {
    writer: Slot,
    rx: Receiver<NetEvent>,
}

impl Link {
    /// Waits for guests on `listener`, one at a time. When a guest leaves the board stays open for the next one
    pub fn host_direct(listener: TcpListener) -> Self {
        let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE);
        let writer: Slot = Arc::default();
        let slot = Arc::clone(&writer);
        // `busy` (not the writer slot) decides whether a guest is in: a failed send empties the slot while the reader may still be
        // running. It is a mutex so `PeerLeft` and the release happen as one step: whoever reconnects on seeing `PeerLeft` finds 
        // the board free, and the next `PeerJoined` can never overtake it. The UI never takes this lock, so holding it across a send to 
        // the bounded queue cannot deadlock
        let busy = Arc::new(Mutex::new(false));
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                if *lock(&busy) {
                    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                    let _ = protocol::write_msg(
                        &mut &stream, 
                        &Msg::Error {
                            reason: "someone is already drawing on this board".into(),
                        },
                    );
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let Ok(reader) = prepare(&stream) else { continue };
                *lock(&busy) = true;
                *lock(&slot) = Some(stream);
                if tx.send(NetEvent::Msg(Msg::PeerJoined)).is_err() {
                    return;
                }
                let (tx, slot, busy) = (tx.clone(), Arc::clone(&slot), Arc::clone(&busy));
                thread::spawn(move || {
                    pump(reader, &tx, false);
                    if let Some(s) = lock(&slot).take() {
                        let _ = s.shutdown(Shutdown::Both);
                    }
                    let mut busy = lock(&busy);
                    let _ = tx.send(NetEvent::Msg(Msg::PeerLeft));
                    *busy = false;
                });
            }
        });
        Self { writer, rx }
    }

    /// Connects straight to a host started with `blackboard host`
    pub fn join_direct(addr: &str) -> Result<Self> {
        Ok(Self::spawn(connect(addr)?, false))
    }

    /// Opens a new room on the relay; its code arrives as `Msg::Room`
    pub fn relay_host(relay: &str) -> Result<Self> {
        let stream = connect(relay)?;
        protocol::write_msg(&mut &stream, &Msg::Create).context("talking to the relay")?;
        Ok(Self::spawn(stream, true))
    }

    /// Joins the room `code` on the relay
    pub fn relay_join(relay: &str, code: &str) -> Result<Self> {
        let stream = connect(relay)?;
        protocol::write_msg(
            &mut &stream, 
            &Msg::Join { 
                code: code.trim().to_ascii_uppercase(),
            },
        )
        .context("talking to the relay")?;
        Ok(Self::spawn(stream, true))
    }

    /// Runs a reader thread over an already connected stream. `via_relay` decides who produces `PeerJoined`: the relay
    /// or us right now
    fn spawn(stream: TcpStream, via_relay: bool) -> Self {
        let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE);
        let writer: Slot = Arc::default();
        let slot = Arc::clone(&writer);
        match prepare(&stream) {
            Ok(reader) => {
                *lock(&writer) = Some(stream);
                if !via_relay {
                    let _ = tx.send(NetEvent::Msg(Msg::PeerJoined));
                }
                thread::spawn(move || {
                    let reason = pump(reader, &tx, via_relay);
                    if let Some(s) = lock(&slot).take() {
                        let _ = s.shutdown(Shutdown::Both);
                    }
                    if !via_relay {
                        let _ = tx.send(NetEvent::Msg(Msg::PeerLeft));
                    }
                    let _ = tx.send(NetEvent::Closed(reason));
                });
            }
            Err(e) => {
                let _ = tx.send(NetEvent::Closed(e.to_string()));
            }
        }
        Self { writer, rx }
    }

    /// Sends `msg` to the peer. Returns false if there is nobody to send to; a failed write closes the connection and the
    /// reader reports why
    pub fn send(&self, msg: &Msg) -> bool {
        let mut slot = lock(&self.writer);
        let Some(stream) = slot.as_ref() else { return false };
        if protocol::write_msg(&mut &*stream, msg).is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            *slot = None;
            return false;
        }
        true
    }

    pub fn try_recv(&self) -> Option<NetEvent> {
        self.rx.try_recv().ok()
    }

    /// Blocks up to `timeout` for the next event. Meant for tests and tools
    pub fn recv_timeout(&self, timeout: Duration) -> Option<NetEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    pub fn close(&self) {
        if let Some(s) = lock(&self.writer).take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

fn connect(addr: &str) -> Result<TcpStream> {
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {addr}"))?
        .collect();
    let mut last = anyhow!("{addr} resolved to no address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = anyhow!(e).context(format!("cannot connect to {a}")),
        }
    }
    Err(last)
}

fn prepare(stream: &TcpStream) -> std::io::Result<BufReader<TcpStream>> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(None)?;
    Ok(BufReader::new(stream.try_clone()?))
}

/// Forwards decoded messages to `tx` until the stream ends; returns why it ended. Without a realay in between, control
/// messages can only come from a misbehaving peer and are dropped
fn pump(mut reader: BufReader<TcpStream>, tx: &SyncSender<NetEvent>, trust_control: bool) -> String {
    let mut buf = Vec::new();
    loop {
        match protocol::read_msg(&mut reader, &mut buf) {
            Ok(Some(msg)) => {
                // Errors are let through so a refused guest learns why
                if !trust_control && !msg.is_peer_msg() && !matches!(msg, Msg::Error { .. }) {
                    continue;
                }
                if tx.send(NetEvent::Msg(msg)).is_err() {
                    return "closed".into();
                }
            }
            Ok(None) => return "connection closed".into(),
            Err(e) => return format!("connection lost: {e}"),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
