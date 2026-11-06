//! Drawing the app with rataui

use clap::builder::styling::Style;
use ratatui::{Frame, buffer::Buffer, layout::{Constraint, Layout, Rect}, style::{Color, Modifier}, symbols::border::EMPTY, text::{Line, Span}, widgets::{Block, BorderType, Widget}};

use crate::{app::{App, Focus}, board::{HEIGHT, WIDTH}};


// Chalkboard green
const SLATE: Color = Color::Rgb(28, 54, 40);
const CHAT_MIN_WIDTH : u16 = 28;

pub fn color_of(c: u8) -> Color {
    match c {
        1 => Color::White,
        2 => Color::LightRed,
        3 => Color::Indexed(208),
        4 => Color::Yellow,
        5 => Color::LightGreen,
        6 => Color::LightCyan,
        7 => Color::LightBlue,
        8 => Color::LightMagenta,
        _ => SLATE,
    }
}

pub fn color_name(c: u8) -> &'static str {
    match c {
        1 => "white",
        2 => "red",
        3 => "orange",
        4 => "yellow",
        5 => "green",
        6 => "cyan",
        7 => "blue",
        8 => "pink",
        _ => "eraser",
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    let board_outer = (WIDTH + 2, HEIGHT + 2);

    // Chat on the right when it fits, below the board otherwise
    let (board_area, chat_area) = if main.width >= board_outer.0 + CHAT_MIN_WIDTH {
        let [b, c] = Layout::horizontal([Constraint::Length(board_outer.0), Constraint::Min(CHAT_MIN_WIDTH)]).areas(main);
        let [b, _] = Layout::vertical([Constraint::Length(board_outer.1), Constraint::Min(0)]).areas(b);
        (b, c)
    } else {
        let [b, c] = Layout::vertical([Constraint::Max(board_outer.1), Constraint::Min(6)]).areas(main);
        (b, c)
    };

    draw_board(frame, app, board_area);
    // draw_chat(frame, app, chat_area);
    // draw_status(frame, app, status);    

}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

fn draw_board(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut title = vec![Span::raw(" blackboard ")];
    if let Some(code) = &app.room {
        title.push(Span::styled(format!("· room {code} "), Style::new().fg(Color::Yellow)));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focus_style(app.focus == Focus::Board))
        .title(Line::from(title));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < WIDTH || inner.height < HEIGHT {
        let note = Line::styled(
            " enlarge the terminal to see the whoel board ", 
            Style::new().fg(Color::LightRed)
        );
        frame.render_widget(
            note, 
        Rect {
                y: area.bottom().saturating_sub(1),
                height: 1.min(area.height),
                ..area            
            },
        );
    }
    let inner = Rect {
        width: inner.width.min(WIDTH),
        height: inner.height.min(HEIGHT),
        ..inner
    };
    app.board_rect = inner;
    frame.render_widget(BoardView { app }, inner);
}

struct BoardView<'a> {
    app: &'a App,
}

impl Widget for BoardView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let app = self.app;
        for y in 0..area.height {
            for x in 0..area.height {
                for x in 0..area.width {
                    let c = app.board.get(x, y);
                    let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else {
                        continue;
                    };
                    if c == EMPTY {
                        cell.set_symbol(" ").set_style(Style::new().bg(SLATE));
                    } else {
                        cell.set_symbol("█").set_style(Style::new().fg(color_of(c)).bg(SLATE));
                    }
                }
            }

            if let Some((x, y)) = app.peer_cursor && x < area.width && y < area.height && let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                cell.set_symbol("+").set_style(
                    Style::new()
                        .fg(Color::LightYellow)
                        .bg(SLATE)
                        .add_modifier(Modifier::BOLD)   
                );
            }
            let (x, y) = app.cursor;
            if app.focus == Focus::Board && x < area.width && y < area.height && let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
            }
        }
    }
}