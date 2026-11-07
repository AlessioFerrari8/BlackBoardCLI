//! Relay server: pairs two clients by room code and forward their messages
//! 
//! The host sends `Create` and gets back `Room { code }`; the geust sends `Join { code }`. Both then receive
//! `PeerJoined` and from that point every peer message one sends is passed to the other. When either side leaves
//! the other gets `PeerLeft` and both connections are closed

use std::{collections::HashMap, io::{self, BufRead, BufReader, BufWriter}, net::{Shutdown, TcpListener, TcpStream}, sync::{Arc, Mutex}, thread, time::Duration};

use crate::protocol::{self, Msg};

const CODE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 6;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub max_rooms: usize,
    // Time a client has 
    pub handshake_timeout: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_rooms: 1000,
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

struct Waiting {
    host: TcpStream,
}

struct State {
    cfg: RelayConfig,
    rooms: Mutex<HashMap<String, Waiting>>,
}

/// Serves forever on `listener`. Logs one line per event to stderr
pub fn serve(listener: TcpListener, cfg: RelayConfig) -> io::Result<()> {
    let state = Arc::new(State {
        cfg, 
        rooms: Mutex::new(HashMap::new()),
    });
    eprintln!("realy listening on {}", listener.local_addr()?);

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

fn handle(stream: TcpStream, state: &Arc<State>) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(state.cfg.handshake_timeout))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut buf = Vec::new();
    let first = protocol::read_msg(&mut reader, &mut buf)?;
    stream.set_read_timeout(None)?;

    // A BufReader may already hold bytes the client sent right after its first line; we cannot hand those to antoher thread's
    // fresh reader, so clients must wait for a reply before sending more (ours do)
    if !reader.buffer().is_empty() {
        refuse(stream, "send nothing before the relay answers");
        return Ok(());
    }

    match first {
        Some(Msg::Create) => {
            let code = {
                let mut rooms = lock(&state.rooms);
                if rooms.len() >= state.cfg.max_rooms {
                    drop(rooms);
                    refuse(stream, "the relay is full, try again later");
                    return Ok(());
                }
                let code = loop {
                    let c = new_code();
                    if !rooms.contains_key(&c) {
                        break c;
                    }
                };
                rooms.insert(
                    code.clone(),
                    Waiting {
                        host: stream.try_clone()?,
                    },
                );
                code
            };
            protocol::write_msg(&mut &stream, &Msg::Room { code: code.clone() })?;
            eprintln!("room {code} created");
            Ok(())
        }
        Some(Msg::Join { code }) => {
            let code = code.trim().to_ascii_uppercase();
            let host = lock(&state.rooms).remove(&code);
            let Some(Waiting { host, .. }) = host else {
                refuse(stream, "no room with that code (it may have expired)");
                return Ok(());
            };
            eprintln!("room {code} paired");
            pair(host, stream)?;
            eprintln!("room {code} closed");
            Ok(())
        }
        Some(_) => {
            refuse(stream, "expected creat or join");
            Ok(())
        }
        None => Ok(()),
    }
}

fn pair(host: TcpStream, guest: TcpStream) -> io::Result<()> {
    for s in [&host, &guest] {
        protocol::write_msg(&mut &*s, &Msg::PeerJoined)?;
    }
    let (h2, g2) = (host.try_clone()?, guest.try_clone()?);
    let forward = thread::spawn(move || pipe(h2, g2));
    pipe(guest, host);
    let _ = forward.join();
    Ok(())
}

/// Forwards peer messages from `src` to `dst` until either side fails, then tells `dst` its peer left and shuts
/// both sockets down
fn pipe(src: TcpStream, dst: TcpStream) {
    let mut reader = BufReader::new(&src);
    let mut writer = BufWriter::new(&dst);
    let mut buf = Vec::new();
    while let Ok(Some(msg)) = protocol::read_msg(&mut reader, &mut buf) {
        if !msg.is_peer_msg() {
            continue;
        }
        if protocol::write_msg(&mut writer, &msg).is_err() {
            break;
        }
    }
    let _ = protocol::write_msg(&mut writer, &Msg::PeerLeft);
    let _ = src.shutdown(Shutdown::Both);
    let _ = dst.shutdown(Shutdown::Both);
}

fn refuse(stream: TcpStream, reason: &str) {
    let _ = protocol::write_msg(&mut &stream, &Msg::Error { reason: reason.into() });
    let _ = stream.shutdown(Shutdown::Both);
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
        }
    }
}