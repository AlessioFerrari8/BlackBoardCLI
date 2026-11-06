//! Application state and input handling. No I/O happens here: network events come in through [`App::on_net`], terminal events
//! through `on_key` and `on_mouse`, and outgoing messages pile up until [`App::take_outbox`]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{board::{self, Board, HEIGHT, PALETTE_LEN, WIDTH}, net::NetEvent, protocol::{MAX_CHAT, MAX_NAME, MAX_POINTS, Msg, PROTO_VERSION, sanitize}};


pub const MAX_BRUSH: u16 = 5;
const CHAT_HISTORY: usize = 500;
const MAX_ROOM_CODE: usize = 16;
const MAX_CLOSE_REASON: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Owns the board: sends it to whoever joins
    Host,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Board,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    Me, 
    Peer,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub who: Who,
    pub text: String,
}

#[derive(Debug)]
pub struct App {
    pub board: Board,
    pub role: Role,
    pub name: String,
    pub peer_name: Option<String>,
    pub connected: bool,
    // The link is gone for good (guest side, or a relay room that closed)
    pub closed: bool,
    pub room: Option<String>,
    // Command a guest runs to join, shown next to the room code
    pub join_cmd: String,
    pub chat: Vec<ChatLine>,
    pub input: String,
    pub focus: Focus,
    // Current chalk; 0 is the eraser
    pub color: u8,
    pub brush: u16,
    // Keyboard cursor on the board
    pub cursor: (u16, u16),
    pub peer_cursor: Option<(u16, u16)>,
    /// Screen area the board is drawn in; set by the UI on every frame and used to turn mouse positions into board cells
    pub board_rect: Rect,
    pub quit: bool,
    drag: Option<((u16, u16), u8)>,
    last_sent_cursor: Option<(u16, u16)>,
    outbox: Vec<Msg>,
}

impl App {
    pub fn new(role: Role, name: &str) -> Self {
        let mut name = sanitize(name, MAX_NAME);
        if name.is_empty() {
            name = "anon".into();
        }
        Self {
            board: Board::new(),
            role,
            name,
            peer_name: None,
            connected: false,
            closed: false,
            room: None,
            join_cmd: "blackboard join".into(),
            chat: Vec::new(),
            input: String::new(),
            focus: Focus::Board,
            color: 1,
            brush: 1,
            cursor: (WIDTH / 2, HEIGHT / 2),
            peer_cursor: None,
            board_rect: Rect::default(),
            quit: false,
            drag: None,
            last_sent_cursor: None,
            outbox: Vec::new(),
        }
    }

    pub fn take_outbox(&mut self) -> Vec<Msg> {
        std::mem::take(&mut self.outbox)
    }

    pub fn system(&mut self, text: impl Into<String>) {
        self.push_chat(Who::System, text.into());
    }

    pub fn peer_label(&self) -> &str {
        self.peer_name.as_deref().unwrap_or("your friend")
    }

    // NETWORK

    pub fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Closed(reason) => {
                if !self.closed {
                    self.system(format!("disconnected: {}", sanitize(&reason, MAX_CLOSE_REASON)));
                }
                self.closed = true;
                self.connected = false;
                self.peer_cursor = None;
            }
            NetEvent::Msg(msg) => self.on_msg(msg),
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Room { code } => {
                let code = sanitize(&code, MAX_ROOM_CODE);
                self.system(format!(
                    "room {code} is open, your friend runs: {} {code}",
                    self.join_cmd
                ));
                self.room = Some(code);
            }
            Msg::PeerJoined => {
                self.connected = true;
                self.last_sent_cursor = None;
                self.emit(Msg::Hello {
                    name: self.name.clone(),
                    proto: PROTO_VERSION,
                });
                if self.role == Role::Host {
                    self.emit(Msg::Snapshot {
                        w: WIDTH,
                        h: HEIGHT,
                        runs: self.board.to_runs(),
                    });
                }
            }
            Msg::PeerLeft => {
                if self.connected {
                    self.system(format!("{} left", self.peer_label()));
                }
                self.connected = false;
                self.peer_name = None;
                self.peer_cursor = None;
                if self.role == Role::Host && self.room.is_none() {
                    self.system("waiting for someone else to join");
                }
            }
            Msg::Error { reason } => self.system(format!("error: {}", sanitize(&reason, 200))),
            Msg::Hello { name, proto } => {
                let name = sanitize(&name, MAX_NAME);
                self.peer_name = Some(if name.is_empty() { "anon".into() } else { name });
                self.system(format!("{} joined", self.peer_label()));
                if proto != PROTO_VERSION {
                    self.system(format!(
                        "warning: they run protocol v{proto}, you run v{PROTO_VERSION}; update both to the same version"
                    ));
                }
            }
            Msg::Draw { color, points } => self.board.paint(color, &points),
            Msg::Clear => {
                self.board.clear();
                self.system(format!("{} cleared the board", self.peer_label()));
            }
            Msg::Chat { text } => {
                let text = sanitize(&text, MAX_CHAT);
                if !text.is_empty() {
                    self.push_chat(Who::Peer, text);
                }
            }
            Msg::Cursor { x, y } => self.peer_cursor = Board::in_bounds(x, y).then_some((x, y)),
            Msg::Snapshot { w, h, runs } => match Board::from_runs(w, h, &runs) {
                Some(b) => self.board = b,
                None => self.system("error: received a board that does not fit, ignored it"),
            },
            Msg::Create | Msg::Join { .. } => {}
        }
    }

    // KEYBOARD
    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit = true;
            return;
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            self.focus = match self.focus {
                Focus::Board => Focus::Chat,
                Focus::Chat => Focus::Board,
            };
            return;
        }
        match self.focus {
            Focus::Chat => self.chat_key(key),
            Focus::Board => self.board_key(key),
        }
    }

    fn chat_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.focus = Focus::Board,
            KeyCode::Enter => {
                let text = sanitize(&self.input, MAX_CHAT);
                self.input.clear();
                if text.is_empty() {
                    return;
                }
                self.push_chat(Who::Me, text.clone());
                if !self.emit(Msg::Chat { text }) {
                    self.system("nobody is connected yet, message not sent");
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) if !c.is_control() && self.input.chars().count() < MAX_CHAT => self.input.push(c),
            _ => {}
        }
    }

    fn board_key(&mut self, key: KeyEvent) {
        let (x, y) = self.cursor;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Enter | KeyCode::Char('t') => self.focus = Focus::Chat,
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(x.saturating_sub(1), y),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor((x + 1).min(WIDTH - 1), y),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(x, y.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(x, (y + 1).min(HEIGHT - 1)),
            KeyCode::Char(' ') => self.stroke(self.color, &[self.cursor]),
            KeyCode::Char('x') => self.stroke(board::EMPTY, &[self.cursor]),
            KeyCode::Char(d @ '0' ..='9') => {
                let n = d as u8 - b'0';
                if n <= PALETTE_LEN {
                    self.color = n;
                }
            }            
            KeyCode::Char('e') => self.color = board::EMPTY,
            KeyCode::Char('+') | KeyCode::Char('=') => self.brush = (self.brush + 1).min(MAX_BRUSH),
            KeyCode::Char('-') => self.brush = self.brush.saturating_sub(1).max(1),
            KeyCode::Char('C') => {
                self.board.clear();
                self.emit(Msg::Clear);
                self.system("you cleared the board");
            }
            _ => {}
        }
    }

    fn move_cursor(&mut self, x: u16, y: u16) {
        self.cursor = (x, y);
        self.send_cursor((x, y));
    }

    // MOUSE
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        let cell = self.cell_at(ev.column, ev.row);
        match ev.kind {
            // DOWN, UP, DRAG, MOVED
            MouseEventKind::Down(button) => {
                let Some(p) = cell else { return };
                let color = match button {
                    MouseButton::Left => self.color,
                    MouseButton::Right => board::EMPTY,
                    MouseButton::Middle => return,
                };
                self.focus = Focus::Board;
                self.cursor = p;
                self.drag = Some((p, color));
                self.stroke(color, &[p]);
                self.send_cursor(p);
            }
            MouseEventKind::Drag(_) => {
                let (Some(p), Some((last, color))) = (cell, self.drag) else {
                    return;
                };
                if p != last {
                    self.stroke(color, &board::line(last, p));
                    self.drag = Some((p, color));
                    self.cursor = p;
                    self.send_cursor(p);
                }
            }
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::Moved => {
                if let Some(p) = cell {
                    self.send_cursor(p);
                }
            }
            _ => {}
        }
    }

    /// Board cell under a terminal position
    pub fn cell_at(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        let r = self.board_rect;
        if col < r.x || row < r.y || col >= r.x + r.width || row >= r.y + r.height {
            return None;
        }
        let (x, y) = (col - r.x , row - r.y);
        Board::in_bounds(x, y).then_some((x, y))
    }

    // helpers
    fn stroke(&mut self, color: u8, centres: &[(u16, u16)]) {
        let points = board::brush(centres, self.brush);
        self.board.paint(color, &points);
        for chunk in points.chunks(MAX_POINTS) {
            self.emit(Msg::Draw { 
                color, 
                points: chunk.to_vec(), 
            });
        }
    }

    fn send_cursor(&mut self, p: (u16, u16)) {
        if self.last_sent_cursor != Some(p) && self.emit(Msg::Cursor { x: p.0, y: p.1 }) {
            self.last_sent_cursor = Some(p);
        }
    }    

    /// Queues `msg` for the peer. Nothing is quedued, while alone: the relay would refuse traffic before pairing, and a 
    /// late guest gets a snapshot
    fn emit(&mut self, msg: Msg) -> bool {
        if self.connected {
            self.outbox.push(msg);
        }
        self.connected
    }

    fn push_chat(&mut self, who: Who, text: String) {
        self.chat.push(ChatLine { who, text });
        if self.chat.len() > CHAT_HISTORY {
            let extra = self.chat.len() - CHAT_HISTORY;
            self.chat.drain(..extra);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn connected(role: Role) -> App {
        let mut app = App::new(role, "me");
        app.on_net(NetEvent::Msg(Msg::PeerJoined));
        app.take_outbox();
        app
    }

    #[test]
    fn nothing_is_sent_while_alone() {
        let mut app = App::new(Role::Host, "me");
        app.on_key(key(KeyCode::Char(' ')));
        app.on_key(key(KeyCode::Right));
        assert!(app.take_outbox().is_empty());
        assert!(!app.board.is_empty(), "drawing still works offline");
    }

    #[test]
    fn keyboard_painting_and_colors() {
        let mut app = connected(Role::Guest);
        app.on_key(key(KeyCode::Char('4')));
        app.on_key(key(KeyCode::Char(' ')));
        let (x, y) = app.cursor;
        assert_eq!(app.board.get(x, y), 4);
        assert_eq!(
            app.take_outbox(),
            vec![Msg::Draw { 
                color: 4, 
                points: vec![[x, y]]
            }]
        );
        app.on_key(key(KeyCode::Char('9')));
        assert_eq!(app.color, 4, "9 is outside the palette");
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.board.get(x, y), 0);
    }

}