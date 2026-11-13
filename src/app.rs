//! Application state and input handling. No I/O happens here: network events come in through [`App::on_net`], terminal events
//! through `on_key` and `on_mouse`, and outgoing messages pile up until [`App::take_outbox`]

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    board::{self, Board, Cell, HEIGHT, PALETTE_LEN, WIDTH},
    net::NetEvent,
    protocol::{MAX_CHAT, MAX_NAME, MAX_POINTS, Msg, PROTO_VERSION, sanitize},
};

pub const MAX_BRUSH: u16 = 5;
const CHAT_HISTORY: usize = 500;
const MAX_ROOM_CODE: usize = 16;
const MAX_CLOSE_REASON: usize = 200;

/// Strokes you can take back with undo
const UNDO_HISTORY: usize = 100;

/// One undoable stroke: for each cell it touched, what was there before and what the stroke left
type Stroke = HashMap<(u16, u16), (Cell, Cell)>;

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

/// What a press on the board does
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    // Free hand: paints where you drag
    Pen,
    // Press at one end, release at the other
    Line,
    // Press at one corner, release at the opposite one
    Rect,
    // Click and type letters on the board
    Text,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pen => "pen",
            Tool::Line => "line",
            Tool::Rect => "rect",
            Tool::Text => "text",
        }
    }
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
    pub tool: Tool,
    // Where the line or rectangle being drawn starts, with its color. It ends at the cursor
    pub anchor: Option<((u16, u16), u8)>,
    // While typing on the board: the column a new line goes back to. The next letter goes at the cursor
    pub typing: Option<u16>,
    // Keyboard cursor on the board
    pub cursor: (u16, u16),
    pub peer_cursor: Option<(u16, u16)>,
    // Screen area the board is drawn in; set by the UI on every frame and used to turn mouse positions into board cells
    pub board_rect: Rect,
    pub quit: bool,
    drag: Option<((u16, u16), u8)>,
    /// The stroke being drawn (a mouse drag, a typing session), still open
    open: Option<Stroke>,
    undo: Vec<Stroke>,
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
            tool: Tool::Pen,
            anchor: None,
            typing: None,
            cursor: (WIDTH / 2, HEIGHT / 2),
            peer_cursor: None,
            board_rect: Rect::default(),
            quit: false,
            drag: None,
            open: None,
            undo: Vec::new(),
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
                        glyphs: self.board.glyph_list(),
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
                self.forget_undo();
                self.system(format!("{} cleared the board", self.peer_label()));
            }
            Msg::Chat { text } => {
                let text = sanitize(&text, MAX_CHAT);
                if !text.is_empty() {
                    self.push_chat(Who::Peer, text);
                }
            }
            Msg::Cursor { x, y } => self.peer_cursor = Board::in_bounds(x, y).then_some((x, y)),
            Msg::Text { color, x, y, text } => {
                // `write` skips anything that is not a letter, escapes included, and stops at the edge
                let text: String = text.chars().take(WIDTH as usize).collect();
                self.board.write(color, x, y, &text);
            }
            Msg::Snapshot { w, h, runs, glyphs } => match Board::from_runs(w, h, &runs) {
                Some(mut b) => {
                    b.add_glyphs(&glyphs);
                    self.board = b;
                    self.forget_undo();
                } 
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
            self.stop_typing();
            self.focus = match self.focus {
                Focus::Board => Focus::Chat,
                Focus::Chat => Focus::Board,
            };
            return;
        }
        match self.focus {
            Focus::Chat => self.chat_key(key),
            Focus::Board if self.typing.is_some() => self.text_key(key),
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
            KeyCode::Char(' ') => self.press(self.color),
            KeyCode::Char('x') => self.press(board::EMPTY),
            KeyCode::Esc => self.anchor = None,
            KeyCode::Char('P') => self.set_tool(Tool::Pen),
            KeyCode::Char('L') => self.set_tool(Tool::Line),
            KeyCode::Char('R') => self.set_tool(Tool::Rect),
            KeyCode::Char('T') => self.set_tool(Tool::Text),
            KeyCode::Char(d @ '0'..='9') => {
                let n = d as u8 - b'0';
                if n <= PALETTE_LEN {
                    self.color = n;
                }
            }
            KeyCode::Char('e') => self.color = board::EMPTY,
            KeyCode::Char('+') | KeyCode::Char('=') => self.brush = (self.brush + 1).min(MAX_BRUSH),
            KeyCode::Char('-') => self.brush = self.brush.saturating_sub(1).max(1),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => self.undo(),
            KeyCode::Char('C') => {
                self.board.clear();
                self.forget_undo();
                self.emit(Msg::Clear);
                self.system("you cleared the board");
            }
            _ => {}
        }
    }

    fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.anchor = None;
        self.stop_typing();
    }
    
    /// Space or x on the keyboard: the pen paints right away, a shape takes two presses, one per end, text starts writing
    fn press(&mut self, color: u8) {
        match (self.tool, self.anchor.take()) {
            (Tool::Pen, _) => self.stroke(color, &[self.cursor]),
            (Tool::Text, _) => self.start_typing(self.cursor),
            (_, Some((start, color))) => self.finish_shape(start, color),
            (_, None) => self.anchor = Some((self.cursor, color)),   
        }
    }

    /// Cells the shape in progress would paint, for the UI to show before it is committed
    pub fn preview(&self) -> Option<(u8, Vec<[u16; 2]>)> {
        let (start, color) = self.anchor?;
        Some((color, board::brush(&self.shape(start, self.cursor), self.brush)))
    }

    fn shape(&self, a: (u16, u16), b: (u16, u16)) -> Vec<(u16, u16)> {
        match self.tool {
            Tool::Rect => board::rect(a, b),
            _ => board::line(a, b),
        }
    }

    fn finish_shape(&mut self, start: (u16, u16), color: u8) {
        let cells = self.shape(start, self.cursor);
        self.stroke(color, &cells);
    }

    fn start_typing(&mut self, at: (u16, u16)) {
        if self.color == board::EMPTY {
            self.system("pick a chalk (1-8) to write with");
            return;
        }
        self.stop_typing();
        self.cursor = at;
        self.typing = Some(at.0);
        // The whole typing session is one stroke for undo
        self.close_stroke();
        self.open = Some(Stroke::new());
    }

    fn stop_typing(&mut self) {
        if self.typing.take().is_some() {
            self.close_stroke()
        }
    }

    /// Keys while typing on the board: letters go in and the cursor moves right, Enter starts a new line under the first letter,
    /// Esc just stops
    fn text_key(&mut self, key: KeyEvent) {
        let Some(start) = self.typing else { return };
        let (x, y) = self.cursor;
        match key.code {
            KeyCode::Esc => self.stop_typing(),
            KeyCode::Enter => self.move_cursor(start, (y + 1).min(HEIGHT - 1)),
            KeyCode::Backspace if x > start => {
                self.move_cursor(x - 1, y);
                self.stroke_cells(board::EMPTY, &[[x - 1, y]]);
            }
            KeyCode::Char(c) if board::is_glyph(c) && x < WIDTH => {
                let color = self.color;
                self.record(&[[x, y]], |b| {
                    b.write(color, x, y, &c.to_string());
                });
                self.emit(Msg::Text { 
                    color, 
                    x, 
                    y, 
                    text: c.to_string(),
                });
                self.move_cursor((x + 1).min(WIDTH - 1), y);
            }
            KeyCode::Char(' ') if x < WIDTH => {
                self.stroke_cells(board::EMPTY, &[[x, y]]);
                self.move_cursor((x + 1).min(WIDTH - 1), y)
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
                if self.tool == Tool::Text {
                    if button == MouseButton::Left {
                        self.start_typing(p);
                        self.send_cursor(p);
                    }
                    return;
                }
                self.cursor = p;
                self.drag = Some((p, color));
                if self.tool == Tool::Pen {
                    // Everything until the button goes up is one stroke for undo
                    self.close_stroke();
                    self.open = Some(Stroke::new());
                    self.stroke(color, &[p]);
                } else {
                    self.anchor = Some((p, color));
                }
                self.send_cursor(p);
            }
            MouseEventKind::Drag(_) => {
                let (Some(p), Some((last, color))) = (cell, self.drag) else {
                    return;
                };
                if p != last {
                    if self.tool == Tool::Pen {
                        self.stroke(color, &board::line(last, p));
                    }
                    self.drag = Some((p, color));
                    self.cursor = p;
                    self.send_cursor(p);
                }
            }
            MouseEventKind::Up(_) => {
                if self.tool == Tool::Pen {
                    self.close_stroke();
                }
                // A shape ends where the mouse was released, or at the last cell of the board it was dragged over
                if self.drag.take().is_some() && let Some((start, color)) = self.anchor.take() {
                    if let Some(p) = cell {
                        self.cursor = p;
                    }
                    self.finish_shape(start, color);
                }
            } 
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
        let (x, y) = (col - r.x, row - r.y);
        Board::in_bounds(x, y).then_some((x, y))
    }

    // helpers
    fn stroke(&mut self, color: u8, centres: &[(u16, u16)]) {
        let points = board::brush(centres, self.brush);
        self.stroke_cells(color, &points);
    }

    fn stroke_cells(&mut self, color: u8, points: &[[u16; 2]]) {
        self.record(points, |b| b.paint(color, points));
        for chunk in points.chunks(MAX_POINTS) {
            self.emit(Msg::Draw {
                color,
                points: chunk.to_vec(),
            });
        }
    }

    /// Applies `change` to the board and remembers what it did to `cells`, for undo. Outisde an open stroke the change is 
    /// a stroke of its own
    fn record(&mut self, cells: &[[u16; 2]], change: impl FnOnce(&mut Board)) {
        let before: Vec<Cell> = cells.iter().map(|&[x, y]| self.board.cell(x, y)).collect();
        change(&mut self.board);
        let single = self.open.is_none();
        let stroke = self.open.get_or_insert_with(Stroke::new);
        for (&[x, y], before) in cells.iter().zip(before) {
            let after = self.board.cell(x, y);
            // The first `before` wins: it is what the cell held when the stroke started
            stroke.entry((x, y)).or_insert((before, after)).1 = after;
        }
        if single {
            self.close_stroke();
        }
    }

    fn close_stroke(&mut self) {
        let Some(mut stroke) = self.open.take() else { return };
        stroke.retain(|_, (before, after)| before != after);
        if stroke.is_empty() {
            return;
        }
        self.undo.push(stroke);
        if self.undo.len() > UNDO_HISTORY {
            self.undo.remove(0);
        }
    }
    
    fn forget_undo(&mut self) {
        self.open = None;
        self.undo.clear();
    }

    /// Takes back your last stroke. A cell your friend has drawn over since then is left alone, so undo only removes your
    /// own chalk. The result is sent as ordinary drawing, so the other side needs nothing special
    fn undo(&mut self) {
        self.close_stroke();
        let Some(stroke) = self.undo.pop() else {
            self.system("nothing to undo");
            return;
        };
        let mut by_color: HashMap<u8, Vec<[u16; 2]>> = HashMap::new();
        let mut letters = Vec::new();
        let mut cells: Vec<_> = stroke.into_iter().collect();
        cells.sort_unstable_by_key(|&((x, y), _)| (y, x));
        for ((x, y), (before, after)) in cells {
            if self.board.cell(x, y) != after {
                continue;
            }
            self.board.set(x, y, before);
            match before.glyph {
                Some(c) => letters.push(Msg::Text { 
                    color: before.color, 
                    x, 
                    y, 
                    text: c.to_string(),
                }),
                None => by_color.entry(before.color).or_default().push([x, y]),
            }
        }
        let mut colors: Vec<_> = by_color.into_iter().collect();
        colors.sort_unstable_by_key(|(c, _)| *c);
        for (color, points) in colors {
            for chunk in points.chunks(MAX_POINTS) {
                self.emit(Msg::Draw { color, points: chunk.to_vec(), });
            }
        }
        for msg in letters {
            self.emit(msg);
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

    #[test]
    fn mouse_drag_draws_a_continuous_line() {
        let mut app = connected(Role::Host);
        app.board_rect = Rect::new(10, 5, WIDTH, HEIGHT);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 5));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 15, 5));
        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 15, 5));
        for x in 0..=5 {
            assert_eq!(app.board.get(x, 0), 1);
        }
        let draws = app
            .take_outbox()
            .into_iter()
            .filter(|m| matches!(m, Msg::Draw { .. }))
            .count();
        assert_eq!(draws, 2);
        // A drag without a press must not draw.
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 20, 8));
        assert_eq!(app.board.get(10, 3), 0);
    }

    #[test]
    fn right_button_erases_and_outside_clicks_are_ignored() {
        let mut app = connected(Role::Host);
        app.board_rect = Rect::new(0, 0, 10, 10);
        app.board.paint(2, &[[3, 3]]);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 3, 3));
        assert_eq!(app.board.get(3, 3), 0);
        app.take_outbox();
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 3));
        assert!(app.take_outbox().is_empty());
    }

    #[test]
    fn clear_requires_shift_and_brush_is_bounded() {
        let mut app = connected(Role::Host);
        app.board.paint(1, &[[0, 0]]);
        app.on_key(key(KeyCode::Char('c')));
        assert!(!app.board.is_empty());
        app.on_key(key(KeyCode::Char('C')));
        assert!(app.board.is_empty());
        assert!(app.take_outbox().contains(&Msg::Clear));
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('+')));
        }
        assert_eq!(app.brush, MAX_BRUSH);
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('-')));
        }
        assert_eq!(app.brush, 1);
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        let mut app = App::new(Role::Host, "me");
        app.focus = Focus::Chat;
        let mut k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        k.kind = KeyEventKind::Press;
        app.on_key(k);
        assert!(app.quit);
    }

    #[test]
    fn cursor_updates_are_deduplicated() {
        let mut app = connected(Role::Host);
        app.board_rect = Rect::new(0, 0, WIDTH, HEIGHT);
        app.on_mouse(mouse(MouseEventKind::Moved, 4, 4));
        app.on_mouse(mouse(MouseEventKind::Moved, 4, 4));
        assert_eq!(app.take_outbox(), vec![Msg::Cursor { x: 4, y: 4 }]);
    }

    #[test]
    fn host_greets_and_sends_snapshot_on_join() {
        let mut app = App::new(Role::Host, "ale");
        app.board.paint(3, &[[1, 1]]);
        app.on_net(NetEvent::Msg(Msg::PeerJoined));
        let out = app.take_outbox();
        assert_eq!(
            out[0],
            Msg::Hello {
                name: "ale".into(),
                proto: PROTO_VERSION
            }
        );
        assert!(
            matches!(&out[1], Msg::Snapshot { runs, .. } if Board::from_runs(WIDTH, HEIGHT, runs) == Some(app.board.clone()))
        );
    }

    #[test]
    fn guest_only_greets() {
        let mut app = App::new(Role::Guest, "bob");
        app.on_net(NetEvent::Msg(Msg::PeerJoined));
        assert_eq!(app.take_outbox().len(), 1);
    }

    #[test]
    fn chat_send_and_receive() {
        let mut app = connected(Role::Guest);
        app.on_key(key(KeyCode::Tab));
        for c in "ciao!".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.quit, "q types in chat");
        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_outbox(), vec![Msg::Chat { text: "ciao!".into() }]);
        assert!(app.input.is_empty());

        app.on_net(NetEvent::Msg(Msg::Chat {
            text: "\x1b[31mhey".into(),
        }));
        assert_eq!(
            app.chat.last(),
            Some(&ChatLine {
                who: Who::Peer,
                text: "[31mhey".into()
            })
        );
    }

    #[test]
    fn peer_messages_update_state() {
        let mut app = connected(Role::Guest);
        app.on_net(NetEvent::Msg(Msg::Hello {
            name: "  Ann\n".into(),
            proto: PROTO_VERSION,
        }));
        assert_eq!(app.peer_name.as_deref(), Some("Ann"));
        app.on_net(NetEvent::Msg(Msg::Draw {
            color: 2,
            points: vec![[0, 0], [999, 999]],
        }));
        assert_eq!(app.board.get(0, 0), 2);
        app.on_net(NetEvent::Msg(Msg::Cursor { x: 999, y: 1 }));
        assert_eq!(app.peer_cursor, None);
        app.on_net(NetEvent::Msg(Msg::Clear));
        assert!(app.board.is_empty());
        app.on_net(NetEvent::Msg(Msg::Snapshot {
            w: 3,
            h: 3,
            runs: vec![[1, 9]],
            glyphs: vec![],
        }));
        assert!(app.board.is_empty());
        app.on_net(NetEvent::Msg(Msg::PeerLeft));
        assert!(!app.connected && app.peer_name.is_none());
    }

    #[test]
    fn close_reasons_are_sanitized() {
        // A peer picks part of this text: the decoder quotes unknown tags.
        let mut app = connected(Role::Guest);
        app.on_net(NetEvent::Closed(format!(
            "connection lost: unknown variant `\x1b[2J\u{202E}{}`",
            "x".repeat(60_000)
        )));
        let line = &app.chat.last().unwrap().text;
        assert!(line.starts_with("disconnected: connection lost"));
        assert!(!line.contains('\x1b') && !line.contains('\u{202E}'));
        assert!(line.len() < 300);
    }

    #[test]
    fn room_codes_from_the_relay_are_sanitized() {
        let mut app = App::new(Role::Host, "me");
        app.on_net(NetEvent::Msg(Msg::Room {
            code: format!("AB\u{202E}\x1b[2J{}", "C".repeat(100)),
        }));
        let code = app.room.unwrap();
        assert!(code.starts_with("AB[2J") && code.len() <= MAX_ROOM_CODE);
        assert!(!app.chat.last().unwrap().text.contains('\u{202E}'));
    }
}
