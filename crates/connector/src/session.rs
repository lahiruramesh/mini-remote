use std::collections::HashSet;
use std::io::{self, Read};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use remote_protocol::{
    ClientMessage, InputEvent, MAX_CLIPBOARD_TEXT, MAX_VIDEO_CHUNK, MouseButton, ServerMessage,
    StreamProfile, VideoSource, WindowShortcut, diagnostics::Monitor, read_client, write_server,
    write_video,
};

use crate::{Host, pairing};

const CLIPBOARD_POLL: Duration = Duration::from_millis(350);

enum Event {
    Client(ClientMessage),
    Disconnected,
    CaptureEnded(io::Result<()>),
}

pub fn serve(
    mut socket: TcpStream,
    token: &str,
    token_path: &Path,
    clipboard_allowed: bool,
    desktop_session: &Mutex<()>,
) -> Result<()> {
    socket.set_nonblocking(false)?;
    socket.set_write_timeout(Some(Duration::from_secs(4)))?;
    socket.set_nodelay(true)?;
    let mut handshake = HandshakeReader {
        socket: &mut socket,
        deadline: Instant::now() + Duration::from_secs(4),
    };
    let first_message = read_client(&mut handshake).context("read authentication")?;
    let (candidate, clipboard, profile) = match first_message {
        ClientMessage::Authenticate {
            token,
            clipboard,
            profile,
        } => (token, clipboard, profile),
        ClientMessage::RedeemInvite { code } => {
            if !pairing::consume_invite(token_path, token, &code) {
                return reject(
                    &mut socket,
                    "This pairing code expired or has already been used. Show a new code on the desktop.",
                );
            }
            return write_server(
                &mut socket,
                &ServerMessage::Paired {
                    token: token.to_owned(),
                },
            )
            .context("send paired response");
        }
        _ => return reject(&mut socket, "Authentication required."),
    };
    if !pairing::matches(token, &candidate) {
        return reject(&mut socket, "Pairing token rejected.");
    }
    let Ok(_session) = desktop_session.try_lock() else {
        return reject(
            &mut socket,
            "This desktop already has an active session. Disconnect the other viewer and try again.",
        );
    };
    socket.set_read_timeout(Some(Duration::from_secs(20)))?;

    let mut host = match Host::new() {
        Ok(host) => host,
        Err(error) => return reject(&mut socket, &format!("Desktop unavailable: {error}")),
    };
    let source = match host.start_video(profile) {
        Ok(source) => source,
        Err(error) => return reject(&mut socket, &format!("Screen capture unavailable: {error}")),
    };

    let connected = run_session(
        &mut host,
        &mut socket,
        clipboard_allowed,
        clipboard,
        source,
        profile,
    );
    let _ = socket.shutdown(Shutdown::Both);
    connected
}

struct HandshakeReader<'a> {
    socket: &'a mut TcpStream,
    deadline: Instant,
}

impl Read for HandshakeReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "authentication timed out",
            ));
        }
        self.socket.set_read_timeout(Some(remaining))?;
        self.socket.read(buffer)
    }
}

fn run_session(
    host: &mut Host,
    socket: &mut TcpStream,
    clipboard_allowed: bool,
    clipboard_requested: bool,
    source: VideoSource,
    profile: StreamProfile,
) -> Result<()> {
    let VideoSource {
        width,
        height,
        mut reader,
        stop,
    } = source;
    let clones = socket
        .try_clone()
        .and_then(|rx| socket.try_clone().map(|tx| (rx, tx)));
    let (mut from_client, to_client) = match clones {
        Ok(sockets) => sockets,
        Err(error) => {
            stop();
            return Err(error.into());
        }
    };

    #[cfg(target_os = "macos")]
    let input_available = remote_macos_host::input_permission_granted();
    #[cfg(target_os = "linux")]
    let input_available = true;

    let clipboard_enabled = clipboard_allowed && clipboard_requested;
    let greeting = (|| -> io::Result<()> {
        write_server(
            socket,
            &ServerMessage::Ready {
                width,
                height,
                diagnostics: true,
                profile,
            },
        )?;
        write_server(
            socket,
            &ServerMessage::ClipboardStatus {
                enabled: clipboard_enabled,
            },
        )?;
        write_server(
            socket,
            &ServerMessage::InputStatus {
                enabled: input_available,
            },
        )
    })();
    if let Err(error) = greeting {
        stop();
        return Err(error.into());
    }
    let to_client = Mutex::new(to_client);
    let (event_tx, event_rx) = mpsc::sync_channel(64);
    let (input_tx, video_tx) = (event_tx.clone(), event_tx);

    thread::scope(|scope| {
        scope.spawn(move || {
            while let Ok(message) = read_client(&mut from_client) {
                if input_tx.send(Event::Client(message)).is_err() {
                    return;
                }
            }
            let _ = input_tx.send(Event::Disconnected);
        });

        let writer = &to_client;
        scope.spawn(move || {
            let end = send_video(reader.as_mut(), writer);
            let _ = video_tx.send(Event::CaptureEnded(end));
        });

        let mut state = Control {
            clipboard_enabled,
            input_available,
            ..Control::default()
        };
        let result = handle_events(host, &to_client, &event_rx, clipboard_allowed, &mut state);
        state.release(host);
        drop(event_rx);
        let _ = socket.shutdown(Shutdown::Both);
        stop();
        result
    })
}

fn send_video(reader: &mut (dyn Read + Send), to_client: &Mutex<TcpStream>) -> io::Result<()> {
    let mut buffer = vec![0; MAX_VIDEO_CHUNK];
    loop {
        let length = match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        write_video(&mut *to_client.lock().unwrap(), &buffer[..length])?;
    }
}

fn handle_events(
    host: &mut Host,
    to_client: &Mutex<TcpStream>,
    events: &mpsc::Receiver<Event>,
    clipboard_allowed: bool,
    state: &mut Control,
) -> Result<()> {
    if state.clipboard_enabled {
        state.clipboard = current_clipboard(host);
    }
    let mut next_poll = Instant::now() + CLIPBOARD_POLL;
    loop {
        match events.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(Event::Client(message)) => {
                state.handle(host, to_client, message, clipboard_allowed)?
            }
            Ok(Event::Disconnected) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok(());
            }
            Ok(Event::CaptureEnded(result)) => {
                let _ = send(to_client, &ServerMessage::Error {
                        message: "Screen capture stopped. Check that the host desktop is available, then reconnect.".into(),
                    });
                return result
                    .and(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "video source closed",
                    )))
                    .context("capture ended");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(usage) = state.monitor.as_ref().and_then(Monitor::poll) {
            send(to_client, &ServerMessage::Diagnostics { usage })?;
        }
        if Instant::now() >= next_poll {
            state.poll_clipboard(host, to_client)?;
            next_poll = Instant::now() + CLIPBOARD_POLL;
        }
    }
}

#[derive(Default)]
struct Control {
    buttons: HashSet<MouseButton>,
    keys: Vec<String>,
    clipboard_enabled: bool,
    input_available: bool,
    clipboard: Option<String>,
    warned_about_input: bool,
    monitor: Option<Monitor>,
    last_diagnostic_request: Option<Instant>,
}

impl Control {
    fn handle(
        &mut self,
        host: &mut Host,
        to_client: &Mutex<TcpStream>,
        message: ClientMessage,
        clipboard_allowed: bool,
    ) -> Result<()> {
        match message {
            ClientMessage::Authenticate { .. } | ClientMessage::RedeemInvite { .. } => {
                return Err(anyhow!("duplicate authentication"));
            }
            ClientMessage::Input(event) if self.input_available => {
                match host.input(event.clone()) {
                    Ok(()) => self.record_input(event),
                    Err(error) if !self.warned_about_input => {
                        self.warned_about_input = true;
                        eprintln!("Input backend reported an error: {error}");
                    }
                    Err(_) => {}
                }
            }
            ClientMessage::WindowShortcut { shortcut } if self.input_available => {
                self.window_shortcut(host, shortcut);
            }
            ClientMessage::Clipboard { text } if self.clipboard_enabled => {
                match host.set_clipboard_text(&text) {
                    Ok(()) => self.clipboard = Some(text),
                    Err(_) => eprintln!("Host clipboard write failed."),
                }
            }
            ClientMessage::PasteText { text } if self.input_available && self.clipboard_enabled => {
                if host.set_clipboard_text(&text).is_ok() {
                    self.clipboard = Some(text);
                    #[cfg(target_os = "macos")]
                    let chord: &[&str] = &["MetaLeft", "KeyV"];
                    #[cfg(target_os = "linux")]
                    let chord: &[&str] = &["ControlLeft", "KeyV"];
                    self.apply_chord(host, chord);
                }
            }
            ClientMessage::ClipboardEnabled { enabled } => {
                let enabled = clipboard_allowed && enabled;
                if self.clipboard_enabled != enabled {
                    self.clipboard_enabled = enabled;
                    self.clipboard = if enabled {
                        current_clipboard(host)
                    } else {
                        None
                    };
                }
                send(to_client, &ServerMessage::ClipboardStatus { enabled })?;
            }
            ClientMessage::Ping => send(to_client, &ServerMessage::Pong)?,
            ClientMessage::RequestDiagnostics => {
                if self
                    .last_diagnostic_request
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(850))
                {
                    self.last_diagnostic_request = Some(Instant::now());
                    if self.monitor.is_none() {
                        self.monitor = Monitor::start().ok();
                    }
                    if let Some(monitor) = &self.monitor {
                        monitor.request();
                    } else {
                        send(to_client, &ServerMessage::Diagnostics { usage: None })?;
                    }
                }
            }

            ClientMessage::Clipboard { .. }
            | ClientMessage::PasteText { .. }
            | ClientMessage::Input(_)
            | ClientMessage::WindowShortcut { .. } => {}
        }
        Ok(())
    }

    fn window_shortcut(&self, host: &mut Host, shortcut: WindowShortcut) {
        self.apply_chord(host, window_chord(shortcut));
    }

    fn apply_chord(&self, host: &mut Host, chord: &[&str]) {
        let held_modifiers: Vec<_> = self
            .keys
            .iter()
            .filter(|key| {
                matches!(
                    key.as_str(),
                    "ShiftLeft"
                        | "ShiftRight"
                        | "ControlLeft"
                        | "ControlRight"
                        | "AltLeft"
                        | "AltRight"
                        | "MetaLeft"
                        | "MetaRight"
                )
            })
            .map(String::as_str)
            .collect();

        let mut set_key = |code: &str, pressed| {
            if let Err(error) = host.input(InputEvent::Key {
                code: code.into(),
                pressed,
            }) {
                eprintln!("Window shortcut could not post an input event: {error}");
            }
        };
        for code in held_modifiers.iter().rev() {
            set_key(code, false);
        }
        for code in chord {
            set_key(code, true);
        }
        for code in chord.iter().rev() {
            set_key(code, false);
        }
        for code in held_modifiers {
            set_key(code, true);
        }
    }

    fn record_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key {
                code,
                pressed: true,
            } if !self.keys.contains(&code) => self.keys.push(code),
            InputEvent::Key {
                code,
                pressed: false,
            } => self.keys.retain(|held| *held != code),
            InputEvent::PointerButton {
                button,
                pressed: true,
            } => {
                self.buttons.insert(button);
            }
            InputEvent::PointerButton {
                button,
                pressed: false,
            } => {
                self.buttons.remove(&button);
            }
            _ => {}
        }
    }

    fn poll_clipboard(&mut self, host: &mut Host, to_client: &Mutex<TcpStream>) -> Result<()> {
        if self.clipboard_enabled {
            let current = current_clipboard(host);
            if current != self.clipboard {
                if let Some(text) = current
                    .as_ref()
                    .filter(|text| text.len() <= MAX_CLIPBOARD_TEXT)
                {
                    send(to_client, &ServerMessage::Clipboard { text: text.clone() })?;
                }
                self.clipboard = current;
            }
        }
        Ok(())
    }

    fn release(self, host: &mut Host) {
        for code in self.keys.into_iter().rev() {
            let _ = host.input(InputEvent::Key {
                code,
                pressed: false,
            });
        }
        for button in self.buttons {
            let _ = host.input(InputEvent::PointerButton {
                button,
                pressed: false,
            });
        }
    }
}

fn window_chord(shortcut: WindowShortcut) -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        match shortcut {
            WindowShortcut::NextApp => &["MetaLeft", "Tab"],
            WindowShortcut::PreviousApp => &["MetaLeft", "ShiftLeft", "Tab"],
            WindowShortcut::CloseWindow => &["MetaLeft", "KeyW"],
        }
    }
    #[cfg(target_os = "linux")]
    {
        match shortcut {
            WindowShortcut::NextApp => &["AltLeft", "Tab"],
            WindowShortcut::PreviousApp => &["AltLeft", "ShiftLeft", "Tab"],
            WindowShortcut::CloseWindow => &["AltLeft", "F4"],
        }
    }
}

fn current_clipboard(host: &mut Host) -> Option<String> {
    host.clipboard_text().ok().flatten()
}

fn send(socket: &Mutex<TcpStream>, message: &ServerMessage) -> io::Result<()> {
    write_server(&mut *socket.lock().unwrap(), message)
}

fn reject(socket: &mut TcpStream, message: &str) -> Result<()> {
    write_server(
        socket,
        &ServerMessage::Error {
            message: message.to_owned(),
        },
    )?;
    Ok(())
}
