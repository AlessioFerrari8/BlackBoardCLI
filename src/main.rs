use std::{io::stdout, time::Duration};

use anyhow::Result;
use blackboard::{app::{App, Role}, ui};
use crossterm::{event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind}, execute};

fn main() -> Result<()> {
    run(App::new(Role::Host, "me"))
}

fn run(mut app: App) -> Result<()> {
    let mut terminal = ratatui::init();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        hook(info);
    }));
    execute!(stdout(), EnableMouseCapture)?;

    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(30))? {
                // Drain everything queued so a fast drag is handled in one frame
                loop {
                    match event::read()? {
                        Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                        Event::Mouse(m) => app.on_mouse(m),
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();

    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}