use std::{
    io::stdout, net::{IpAddr, TcpListener, UdpSocket}, path::{Path, PathBuf}, time::Duration,
};

use anyhow::{Context, Result};
use blackboard::{
    app::{App, Role},
    file,
    net::Link,
    relay::{self, RelayConfig},
    ui,
};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
};

use clap::{Args, Parser, Subcommand};

const DEFAULT_PORT: u16 = 7878;
const DEFAULT_RELAY_PORT: u16 = 7879;
/// Most network events handled per frame, so a flooding peer cannot keep the loop away from the keyboard
const MAX_EVENTS_PER_FRAME: usize = 1024;

/// A shared chalkboard and chat for two people, right in the terminal
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a board and wait for a friend to join
    Host {
        /// Port to listen on (direct mode)
        #[arg(long, short, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Address to listen on (direct mode)
        #[arg(long, default_value = "0.0.0.0")]
        bind: IpAddr,
        /// Start from a board saved with S
        #[arg(long, short)]
        open: Option<PathBuf>,
        #[command(flatten)]
        common: Common,
    },
    /// Join a friend's board
    Join {
        /// The host's address (like 192.168.1.19 or 192.168.1.19:7878), or a room code when --relay is set
        target: String,
        #[command(flatten)]
        common: Common,
    },
    /// Run a relay so two people can meet by room code from any network
    Relay {
        #[arg(long, short, default_value_t = DEFAULT_RELAY_PORT)]
        port: u16,
        #[arg(long, default_value = "0.0.0.0")]
        bind: IpAddr,
        /// Most rooms waiting for a guest at the same time
        #[arg(long, default_value_t = 1000)]
        max_rooms: usize,
        /// Most connections from a single ip
        #[arg(long, default_value_t = 8)]
        max_per_ip: usize,
    },
}

#[derive(Args)]
struct Common {
    /// Name shown to your friend
    #[arg(long, short)]
    name: Option<String>,
    /// Meet through this relay (host:port) instead of connecting directly
    #[arg(long, short, env = "BLACKBOARD_RELAY")]
    relay: Option<String>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Relay { port, bind, max_rooms, max_per_ip } => {
            let listener =
                TcpListener::bind((bind, port)).with_context(|| format!("cannot listen on {bind}:{port}"))?;
            let cfg = RelayConfig {
                max_rooms, 
                max_conns_per_ip: max_per_ip,
                ..RelayConfig::default()
            };
            relay::serve(listener, cfg)?;
            Ok(())
        }
        Command::Host { port, bind, open, common } => {
            let mut app = App::new(Role::Host, &name_or_default(common.name));
            if let Some(path) = open {
                app.board = file::load(&path)?;
                app.system(format!("opened {}", path.display()));
            }

            let link = match &common.relay {
                Some(relay) => {
                    let relay = with_port(relay, DEFAULT_RELAY_PORT);
                    app.join_cmd = format!("blackboard join --relay {relay}");
                    app.system(format!("asking {relay} for a room..."));
                    Link::relay_host(&relay)?
                }
                None => {
                    let listener =
                        TcpListener::bind((bind, port)).with_context(|| format!("cannot listen on {bind}:{port}"))?;
                    let ip = lan_ip().map_or_else(|| "<your-ip>".to_string(), |ip| ip.to_string());
                    let target = if port == DEFAULT_PORT {
                        ip
                    } else {
                        format!("{ip}:{port}")
                    };
                    app.system(format!(
                        "waiting on port {port}, your friend runs: blackboard join {target}"
                    ));
                    Link::host_direct(listener)
                }
            };
            run(app, link)
        }
        Command::Join { target, common } => {
            let app = App::new(Role::Guest, &name_or_default(common.name));
            let link = match &common.relay {
                Some(relay) => Link::relay_join(&with_port(relay, DEFAULT_RELAY_PORT), &target)?,
                None => Link::join_direct(&with_port(&target, DEFAULT_PORT))?,
            };
            run(app, link)
        }
    }
}

fn run(mut app: App, link: Link) -> Result<()> {
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
            for _ in 0..MAX_EVENTS_PER_FRAME {
                let Some(ev) = link.try_recv() else { break };
                app.on_net(ev);
            }
            for msg in app.take_outbox() {
                link.send(&msg);
            }
            if std::mem::take(&mut app.save_requested) {
                match file::save(&app.board, Path::new(".")) {
                    Ok((board, text)) => app.system(format!("saved {} and {}", board.display(), text.display())),
                    Err(e) => app.system(format!("cannot save: {e:#}")),
                }
            }
        }
        Ok(())
    })();

    link.close();
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn name_or_default(name: Option<String>) -> String {
    name.or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "anon".into())
}

/// Appends `port` when `addr` does not carry one. It handles bare Ipv6 too
fn with_port(addr: &str, port: u16) -> String {
    let addr = addr.trim();
    if addr.parse::<std::net::SocketAddr>().is_ok() {
        return addr.to_string();
    }
    if let Ok(ip) = addr.parse::<std::net::Ipv6Addr>() {
        return format!("[{ip}]:{port}");
    }
    if addr.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        return addr.to_string();
    }
    format!("{addr}:{port}")
}

/// Best guess of this machine's LAN address. Connecting a UDP socket sends no packet; it only asks the OS which interface
/// it would use
fn lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    sock.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_default_port_only_when_missing() {
        assert_eq!(with_port("192.168.1.2", 7878), "192.168.1.2:7878");
        assert_eq!(with_port("192.168.1.2:9000", 7878), "192.168.1.2:9000");
        assert_eq!(with_port("relay.example.org", 7879), "relay.example.org:7879");
        assert_eq!(with_port("relay.example.org:80", 7879), "relay.example.org:80");
        assert_eq!(with_port("::1", 7878), "[::1]:7878");
        assert_eq!(with_port("[::1]:9000", 7878), "[::1]:9000");
    }
}
