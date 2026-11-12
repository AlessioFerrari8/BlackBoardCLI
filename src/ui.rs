//! Drawing the app with ratatui

use crate::{
    app::{App, Focus, Who},
    board::{EMPTY, HEIGHT, WIDTH},
};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Chalkboard green
const SLATE: Color = Color::Rgb(28, 54, 40);
const CHAT_MIN_WIDTH: u16 = 28;

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
        let [b, c] =
            Layout::horizontal([Constraint::Length(board_outer.0), Constraint::Min(CHAT_MIN_WIDTH)]).areas(main);
        let [b, _] = Layout::vertical([Constraint::Length(board_outer.1), Constraint::Min(0)]).areas(b);
        (b, c)
    } else {
        let [b, c] = Layout::vertical([Constraint::Max(board_outer.1), Constraint::Min(6)]).areas(main);
        (b, c)
    };

    draw_board(frame, app, board_area);
    draw_chat(frame, app, chat_area);
    draw_status(frame, app, status);
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
            " enlarge the terminal to see the whole board ",
            Style::new().fg(Color::LightRed),
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
            for x in 0..area.width {
                let c = app.board.cell(x, y);
                let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else {
                    continue;
                };
                match c.glyph {
                    Some(g) => {
                        let style = Style::new().fg(color_of(c.color)).bg(SLATE).add_modifier(Modifier::BOLD);
                        cell.set_char(g).set_style(style);
                    }
                    None if c.color == EMPTY => {
                        cell.set_symbol(" ").set_style(Style::new().bg(SLATE));
                    }
                    None => {
                        cell.set_symbol("█").set_style(Style::new().fg(color_of(c.color)).bg(SLATE));
                    }
                }
            }
        }

        if let Some((x, y)) = app.peer_cursor
            && x < area.width
            && y < area.height
            && let Some(cell) = buf.cell_mut((area.x + x, area.y + y))
        {
            cell.set_symbol("+").set_style(
                Style::new()
                    .fg(Color::LightYellow)
                    .bg(SLATE)
                    .add_modifier(Modifier::BOLD),
            );
        }
        // The shape being drawn, before it is committed
        if let Some((color, points)) = app.preview() {
            let (symbol, fg) = if color == EMPTY { ("░", Color::Gray) } else { ("▓", color_of(color)) };
            for [x, y] in points {
                if x < area.width && y < area.height && let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                    cell.set_symbol(symbol).set_style(Style::new().fg(fg).bg(SLATE));
                }
            }
        }
        let (x, y) = app.cursor;
        if app.focus == Focus::Board
            && x < area.width
            && y < area.height
            && let Some(cell) = buf.cell_mut((area.x + x, area.y + y))
        {
            cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
        }
    }
}

fn draw_chat(frame: &mut Frame, app: &App, area: Rect) {
    let peer = if app.connected {
        app.peer_label().to_string()
    } else {
        "nobody yet".into()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(focus_style(app.focus == Focus::Chat))
        .title(format!(" chat · {peer}"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let [log_area, input_area] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    // Wrap ourselves so we can keep the newest lines pinned to the bottom
    let width = log_area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for msg in &app.chat {
        let (prefix, style) = match msg.who {
            Who::Me => (format!("{}: ", app.name), Style::new().fg(Color::LightCyan)),
            Who::Peer => (format!("{}: ", app.peer_label()), Style::new().fg(Color::LightGreen)),
            Who::System => ("· ".to_string(), Style::new().fg(Color::DarkGray)),
        };
        for (i, row) in wrap(&format!("{prefix}{}", msg.text), width).into_iter().enumerate() {
            if i == 0 && msg.who != Who::System {
                let mut split = prefix.len().min(row.len());
                while !row.is_char_boundary(split) {
                    split -= 1;
                }
                let (head, tail) = row.split_at(split);
                lines.push(Line::from(vec![
                    Span::styled(head.to_string(), style.add_modifier(Modifier::BOLD)),
                    Span::raw(tail.to_string()),
                ]));
            } else {
                let s = if msg.who == Who::System { style } else { Style::new() };
                lines.push(Line::styled(row, s));
            }
        }
    }
    let skip = lines.len().saturating_sub(log_area.height as usize);
    let visible: Vec<Line> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(visible), log_area);

    // Input line, scrolled so the end of what you type stays visible
    let prompt = "> ";
    let room = (input_area.width as usize).saturating_sub(prompt.width() + 1);
    let mut shown: Vec<char> = Vec::new();
    let mut used = 0;

    for c in app.input.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        shown.push(c);
    }
    let shown: String = shown.into_iter().rev().collect();
    let hint = app.input.is_empty() && app.focus != Focus::Chat;
    let text = if hint { "Tab to chat".to_string() } else { shown };
    let style = if hint {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new()
    };
    frame.render_widget(
        Line::from(vec![
            Span::styled(prompt, Style::new().fg(Color::Yellow)),
            Span::styled(text, style),
        ]),
        input_area,
    );
    if app.focus == Focus::Chat {
        frame.set_cursor_position(Position::new(
            input_area.x + (prompt.width() + used) as u16,
            input_area.y,
        ));
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let (dot, state) = if app.closed {
        (Style::new().fg(Color::LightRed), "offline".to_string())
    } else if app.connected {
        (Style::new().fg(Color::LightGreen), format!("with {}", app.peer_label()))
    } else {
        (Style::new().fg(Color::Yellow), "waiting".to_string())
    };
    let swatch = if app.color == EMPTY {
        Span::styled("░", Style::new().fg(Color::Gray))
    } else {
        Span::styled("██", Style::new().fg(color_of(app.color)))
    };
    let keys = match app.focus {
        Focus::Board if app.typing.is_some() => "type on the board · Enter new line · Backspace delete · Esc done",
        Focus::Board => {
            "drag to draw · right drag erase · P pen L line R rect T text · 1-8 color · 0 eraser · +/- brush · C clear · Tab chat · q quit"
        }
        Focus::Chat => "Enter send · Esc/Tab back to the board · Ctrl + C quit",
    };
    let line = Line::from(vec![
        Span::styled(" ● ", dot),
        Span::raw(format!("{state} ")),
        swatch,
        Span::raw(format!(" {} · {} · brush {} ", color_name(app.color), app.tool.name(), app.brush)),
        Span::styled(keys, Style::new().fg(Color::DarkGray)),
    ]);
    frame.render_widget(line, area);
}

/// Splits `text` into rows at most `width` columns wide, breaking at spaces when possible and mid-word otherwise
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_w = 0;
    for word in text.split(' ') {
        let word_w = word.width();
        let sep = usize::from(!row.is_empty());
        if row_w + sep + word_w <= width {
            if sep == 1 {
                row.push(' ');
            }
            row.push_str(word);
            row_w += sep + word_w;
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            row_w = 0;
        }
        for c in word.chars() {
            let w = c.width().unwrap_or(0);
            if row_w + w > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_w = 0;
            }
            row.push(c);
            row_w += w;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Role;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn wrap_breaks_at_spaces_and_long_words() {
        assert_eq!(wrap("hello world", 20), vec!["hello world"]);
        assert_eq!(wrap("hello world", 7), vec!["hello", "world"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), vec![""]);
        for row in wrap("一二三四五", 4) {
            assert!(row.width() <= 4);
        }
    }

    #[test]
    fn renders_wide_and_narrow_and_maps_mouse() {
        for (w, h) in [(120, 30), (82, 40), (40, 12)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut app = App::new(Role::Host, "me");
            app.board.paint(2, &[[0, 0]]);
            app.system("hello there, this is a long system line that has to wrap somewhere");
            term.draw(|f| draw(f, &mut app)).unwrap();
            let r = app.board_rect;
            assert!(r.width <= WIDTH && r.height <= HEIGHT);
            assert_eq!(app.cell_at(r.x, r.y), Some((0, 0)));
            assert_eq!(term.backend().buffer()[(r.x, r.y)].symbol(), "█")
        }
    }

    #[test]
    fn system_lines_and_wrapped_rows_are_shown() {
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new(Role::Host, "me");
        app.system("waiting on port 7878");
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("waiting on port 7878"));
    }
}
