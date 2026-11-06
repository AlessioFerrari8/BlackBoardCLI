//! Wire protocol: one JSON object per line over TCP.
//! 
//! The same enum carries p2p messages (drawing, chat, ...) and the relay controls messages (room codes, pairing). The relay
//! only forwards the variants for which [`Msg::is_peer_msg`] is true, so a peer can never fake a control message

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};

/// Bumped on every incompatible change to [`Msg`]
pub const PROTO_VERSION: u32 = 1;

/// Longest line accepted from the network, in Bytes. The worst case is a full snapshot of an 80*24 board
pub const MAX_LINE: usize = 64 * 1024;

/// Max points a single `Draw` message may carry
pub const MAX_POINTS: usize = 4096;
pub const MAX_NAME: usize = 24;
pub const MAX_CHAT: usize = 280;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    // p2p
    Hello {
        name: String,
        proto: u32,
    },
    // Paint `points` with `color`; color 0 erases
    Draw {
        color: u8,
        points: Vec<[u16; 2]>,
    },
    Clear,
    Chat {
        text: String,
    },
    Cursor {
        x: u16,
        y: u16,
    },
    // Whole board, run-length encoded as `[color, run]` pairs in row-major order
    Snapshot {
        w: u16,
        h: u16,
        runs: Vec<[u16; 2]>,
    },

    // Client to relay
    Create,
    Join {
        code: String,
    },

    // Relay to client (also synthesised locally in direct mode)
    Room {
        code: String,
    },
    PeerJoined,
    PeerLeft,
    Error {
        reason: String,
    },
}

impl Msg {
    /// True for the messages one peer may send to the other
    pub fn is_peer_msg(&self) -> bool {
        matches!(
            self,
            Msg::Hello { .. }
                | Msg::Draw { .. }
                | Msg::Clear
                | Msg::Chat { .. }
                | Msg::Cursor { .. }
                | Msg::Snapshot { .. }
        )
    }
}

/// Serialises `msg` and writes it as a single line
pub fn write_msg(w: &mut impl Write, msg: &Msg) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Reads one line into `buf` (without the trailing newline), refusing lines longer than `max` bytes so a peer cannot make 
/// us buffer without limit. Returns `Ok(false)` on a clean EOF
pub fn read_line_capped(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> io::Result<bool> {
    buf.clear();
    loop {
        let available = r.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(false);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed mid-line",
            ));
        }
        let (chunk, found) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], Some(i)),
            None => (available, None),
        };
        if buf.len() + chunk.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        buf.extend_from_slice(chunk);
        match found {
            Some(i) => {
                r.consume(i + 1);
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                return Ok(true);
            }
            None => {
                let n = chunk.len();
                r.consume(n);
            }
        }
    }
}

/// Reads and decodes the next message. `Ok(None)` means the peer hung up
pub fn read_msg(r: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<Msg>> {
    if !read_line_capped(r, buf, MAX_LINE)? {
        return Ok(None);
    }
    serde_json::from_slice(buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Strips control characters (escape sequences included, so a peer cannot repaint our terminal), collapses the
/// result and cuts it to `max` chars
pub fn sanitize(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    cleaned.trim().chars().take(max).collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_every_variant() {
        let msgs = vec![
            Msg::Hello { 
                name: "ale".into(), 
                proto: PROTO_VERSION, 
            },
            Msg::Draw { 
                color: 3, 
                points: vec![[1, 2], [3, 4]], 
            },
            Msg::Clear,
            Msg::Chat { text: "ciao".into() },
            Msg::Cursor { x: 5, y: 6 },
            Msg::Snapshot { 
                w: 2, 
                h: 1, 
                runs: vec![[0, 2]], 
            },
            Msg::Create,
            Msg::Join { code: "ABC234".into() },
            Msg::Room { code: "ABC234".into() },
            Msg::PeerJoined,
            Msg::PeerLeft,
            Msg::Error { reason: "nope".into() },
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            write_msg(&mut wire, m).unwrap();
        }
        let mut r = Cursor::new(wire);
        let mut buf = Vec::new();
        for m in &msgs {
            assert_eq!(read_msg(&mut r, &mut buf).unwrap().as_ref(), Some(m));
        }
        assert_eq!(read_msg(&mut r, &mut buf).unwrap(), None);
    }

    #[test]
    fn wire_format_is_tagged() {
        let mut wire = Vec::new();
        write_msg(&mut wire, &Msg::PeerJoined).unwrap();
        assert_eq!(wire, b"{\"t\":\"peer_joined\"}\n");
    }
}