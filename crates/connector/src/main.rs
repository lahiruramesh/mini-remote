#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("The connector supports Linux and macOS.");

mod pairing;
mod session;

use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use remote_protocol::{DEFAULT_PORT, tailnet::Tailnet};

#[cfg(target_os = "linux")]
use remote_linux_host::Host;
#[cfg(target_os = "macos")]
use remote_macos_host::Host;

#[derive(Parser)]
#[command(version, about = "Share this computer's desktop over Tailscale")]
struct Options {
    /// Override the local file that holds the pairing token
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Show the pairing token for this computer, creating one if needed
    Pair {
        /// Replace the old pairing token
        #[arg(long)]
        rotate: bool,
    },
    /// Generate a short-lived mobile pairing invitation
    Invite {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
    #[cfg(target_os = "macos")]
    /// Check the macOS permissions needed to share this desktop
    Permissions {
        /// Ask macOS to present its permission dialogs
        #[arg(long)]
        request: bool,
    },
    /// Listen for remote desktop connections
    Serve {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Allow text clipboard sharing when the viewer requests it
        #[arg(long)]
        clipboard: bool,
    },
}

fn main() -> Result<()> {
    let options = Options::parse();
    let path = match options.token_file {
        Some(path) => path,
        None => pairing::default_path()?,
    };

    match options.action.unwrap_or(Action::Serve {
        port: DEFAULT_PORT,
        clipboard: true,
    }) {
        Action::Pair { rotate } => {
            println!("{}", pairing::create_or_read(&path, rotate)?);
        }
        Action::Invite { port } => {
            if port == 0 {
                bail!("choose a fixed connector port");
            }
            let address = Tailnet::discover()?.local_ipv4()?;
            let hostname = std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|result| result.status.success())
                .map(|result| {
                    String::from_utf8_lossy(&result.stdout)
                        .trim()
                        .chars()
                        .take(64)
                        .collect::<String>()
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Desktop".to_owned());
            let (code, expires_at) = pairing::create_invite(&path)?;
            println!(
                "{}",
                serde_json::json!({"kind": "miniremote_pairing", "version": 1, "host": address, "port": port, "name": hostname, "code": code, "expires_at": expires_at})
            );
        }
        #[cfg(target_os = "macos")]
        Action::Permissions { request } => {
            let screen = remote_macos_host::screen_permission_granted()
                || (request && remote_macos_host::request_screen_permission());
            if request {
                remote_macos_host::request_input_permission();
            }
            let input = remote_macos_host::input_permission_status();
            println!(
                "Screen recording: {}",
                if screen { "granted" } else { "required" }
            );
            println!(
                "Accessibility: {}",
                if input.accessibility_trusted {
                    "granted"
                } else {
                    "required"
                }
            );
            println!(
                "Input event posting: {}",
                if input.post_event_access {
                    "granted"
                } else {
                    "required"
                }
            );
        }
        Action::Serve { port, clipboard } => {
            if port == 0 {
                bail!("choose a fixed connector port");
            }
            pairing::create_or_read(&path, false)?;
            serve(port, &path, clipboard)?;
        }
    }
    Ok(())
}

fn serve(port: u16, token_path: &std::path::Path, clipboard: bool) -> Result<()> {
    let mut last_failure = None;
    let desktop_session = Arc::new(Mutex::new(()));
    loop {
        let result = Tailnet::discover()
            .and_then(|tailnet| tailnet.local_ipv4())
            .and_then(|ip| {
                let listener = TcpListener::bind(SocketAddr::new(ip, port))?;
                listener.set_nonblocking(true)?;
                Ok((listener, ip))
            });
        let (listener, address) = match result {
            Ok(binding) => binding,
            Err(error) => {
                let message = error.to_string();
                if last_failure.as_ref() != Some(&message) {
                    eprintln!("Waiting for Tailscale listener: {message}");
                    last_failure = Some(message);
                }
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        last_failure = None;
        eprintln!("Connector listening on {address}:{port} via Tailscale.");
        if let Err(error) =
            accept_sessions(listener, address, token_path, clipboard, &desktop_session)
        {
            eprintln!("Reopening Tailscale listener: {error:#}");
            thread::sleep(Duration::from_secs(2));
        }
    }
}

fn accept_sessions(
    listener: TcpListener,
    address: IpAddr,
    token_path: &std::path::Path,
    clipboard: bool,
    desktop_session: &Arc<Mutex<()>>,
) -> Result<()> {
    let mut tailnet = Tailnet::discover()?;
    let mut refresh = std::time::Instant::now();
    let mut clients: Vec<thread::JoinHandle<()>> = Vec::new();
    loop {
        if refresh.elapsed() >= Duration::from_secs(5) {
            tailnet = Tailnet::discover()?;
            if tailnet.local_ipv4()? != address {
                bail!("local Tailscale address changed");
            }
            refresh = std::time::Instant::now();
        }
        let (stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(75));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("accept remote desktop connection"),
        };
        clients.retain(|client| !client.is_finished());
        if clients.len() >= 8 || !tailnet.contains_peer(peer.ip()) {
            continue;
        }
        let token_path = token_path.to_owned();
        let desktop_session = Arc::clone(desktop_session);
        clients.push(thread::spawn(move || {
            let result = pairing::read(&token_path).and_then(|token| {
                session::serve(stream, &token, &token_path, clipboard, &desktop_session)
            });
            if let Err(error) = result {
                eprintln!("Session ended: {error:#}");
            }
        }));
    }
}
