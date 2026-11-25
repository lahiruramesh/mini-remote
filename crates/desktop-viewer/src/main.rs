use std::{
    collections::HashSet,
    io::{BufRead, Read, Write},
    net::{Shutdown, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{self, ColorImage, Key, Modifiers, TextureHandle, Vec2};
use remote_protocol::{
    read_server, write_client, ClientMessage, InputEvent, MouseButton, ServerMessage, StreamProfile,
};

mod connection_test;
mod diagnostics;
#[cfg(target_os = "linux")]
mod mac_signin;
mod setup;
mod shortcuts;
mod workspace;

struct Frame {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

#[derive(Default)]
struct Frames {
    latest: Option<Frame>,
    reusable: Option<Vec<u8>>,
}

enum NetEvent {
    Status(String),
    Connected(mpsc::SyncSender<ClientMessage>, Arc<TcpStream>),
    Ready(u32, u32, bool, StreamProfile),
    Diagnostics(Option<remote_protocol::ResourceUsage>),
    Clipboard(String),
    Closed(String),
    ClipboardStatus(bool),
    InputStatus(bool),
    #[cfg(target_os = "linux")]
    MacSignIn {
        message: String,
        active: bool,
    },
}

struct Viewer {
    host: String,
    recent: workspace::RecentHosts,
    setup: Option<setup::Guide>,
    setup_seen: bool,
    connection_check: Option<mpsc::Receiver<Result<String, String>>>,
    connection_report: Option<Result<String, String>>,
    sidebar: bool,
    diagnostics: diagnostics::Diagnostics,
    remote_diagnostics: bool,
    video_bytes: Arc<AtomicU64>,
    displayed_frames: u64,
    port: String,
    token: String,
    clipboard_enabled: bool,
    status: String,
    connected: bool,
    stream_profile: StreamProfile,
    full_screen: bool,
    dimensions: Option<(u32, u32)>,
    frame_cache: Arc<Mutex<Frames>>,
    texture: Option<TextureHandle>,
    image_rect: Option<egui::Rect>,
    tx: Option<mpsc::SyncSender<ClientMessage>>,
    socket: Option<Arc<TcpStream>>,
    network_events: Option<mpsc::Receiver<NetEvent>>,
    #[cfg(target_os = "linux")]
    helper_events: mpsc::Receiver<NetEvent>,
    #[cfg(target_os = "linux")]
    helper_sender: mpsc::Sender<NetEvent>,
    clipboard: Option<arboard::Clipboard>,
    clipboard_ready: bool,
    last_clipboard: String,
    last_clipboard_poll: Instant,
    map_cmd_ctrl: bool,
    shortcuts: shortcuts::Bindings,
    shortcut_draft: Option<shortcuts::Bindings>,
    shortcut_recording: Option<usize>,
    shortcut_keys: HashSet<Key>,
    last_modifiers: Modifiers,
    held_keys: HashSet<&'static str>,
    held_buttons: HashSet<MouseButton>,
    retry_at: Option<Instant>,
    retry_count: u8,
    auto_reconnect: bool,
    startup_deadline: Option<Instant>,
    clipboard_denied: bool,
    input_enabled: bool,
    connect_on_start: bool,
    #[cfg(target_os = "linux")]
    mac_signin_active: bool,
    cancel_connect: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for Viewer {
    fn default() -> Self {
        #[cfg(target_os = "linux")]
        let (helper_sender, helper_events) = mpsc::channel();
        Self {
            host: String::new(),
            recent: workspace::RecentHosts::default(),
            setup: None,
            setup_seen: false,
            connection_check: None,
            connection_report: None,
            sidebar: true,
            diagnostics: diagnostics::Diagnostics::default(),
            remote_diagnostics: false,
            video_bytes: Arc::new(AtomicU64::new(0)),
            displayed_frames: 0,
            port: remote_protocol::DEFAULT_PORT.to_string(),
            token: String::new(),
            clipboard_enabled: false,
            status: "Disconnected".into(),
            connected: false,
            stream_profile: StreamProfile::Desktop,
            full_screen: false,
            dimensions: None,
            frame_cache: Arc::new(Mutex::new(Frames::default())),
            texture: None,
            image_rect: None,
            tx: None,
            socket: None,
            network_events: None,
            #[cfg(target_os = "linux")]
            helper_events,
            #[cfg(target_os = "linux")]
            helper_sender,
            clipboard: None,
            clipboard_ready: false,
            last_clipboard: String::new(),
            last_clipboard_poll: Instant::now(),
            map_cmd_ctrl: true,
            shortcuts: shortcuts::Bindings::default(),
            shortcut_draft: None,
            shortcut_recording: None,
            shortcut_keys: HashSet::new(),
            last_modifiers: Modifiers::default(),
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            retry_at: None,
            retry_count: 0,
            auto_reconnect: true,
            startup_deadline: None,
            clipboard_denied: false,
            input_enabled: false,
            connect_on_start: false,
            #[cfg(target_os = "linux")]
            mac_signin_active: false,
            cancel_connect: None,
        }
    }
}

impl Viewer {
    fn connect(&mut self, context: &egui::Context) {
        if self.connection_check.is_some() {
            return;
        }
        self.input_enabled = false;
        let host = self.host.trim().to_owned();
        let port: u16 = match self.port.parse() {
            Ok(p) => p,
            Err(_) => {
                self.status = "Port must be between 1 and 65535".into();
                return;
            }
        };
        if port == 0 {
            self.status = "Port must be between 1 and 65535".into();
            return;
        }
        if host.is_empty() || self.token.is_empty() {
            self.status = "Enter a Tailscale host and pairing token".into();
            return;
        }
        self.status = "Resolving Tailscale address…".into();
        self.clipboard_ready = false;
        self.clipboard_denied = false;
        if self.clipboard_enabled {
            self.last_clipboard = self.get_clipboard().unwrap_or_default();
        }
        self.frame_cache = Arc::new(Mutex::new(Frames::default()));
        let (tx_events, events) = mpsc::channel();
        self.network_events = Some(events);
        let frame_cache = self.frame_cache.clone();
        self.video_bytes = Arc::new(AtomicU64::new(0));
        let video_bytes = self.video_bytes.clone();
        self.displayed_frames = 0;
        let render_context = context.clone();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.cancel_connect = Some(cancelled.clone());
        let cancelled_worker = cancelled.clone();
        let clipboard = self.clipboard_enabled;
        let token = self.token.trim().to_owned();
        let profile = self.stream_profile;
        thread::spawn(move || {
            let stream = match remote_protocol::tailnet::connect(&host, port) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx_events.send(NetEvent::Closed(format!(
                        "Tailscale connection failed: {e}"
                    )));
                    return;
                }
            };
            if cancelled_worker.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let _ = stream.set_nodelay(true);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(4)));
            let reader = match stream.try_clone() {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx_events
                        .send(NetEvent::Closed(format!("Could not clone connection: {e}")));
                    return;
                }
            };
            let shutdown_socket = match stream.try_clone() {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    let _ = tx_events
                        .send(NetEvent::Closed(format!("Could not clone connection: {e}")));
                    return;
                }
            };
            let writer = stream;
            let (out_tx, out_rx) = mpsc::sync_channel::<ClientMessage>(256);
            if tx_events
                .send(NetEvent::Connected(out_tx, shutdown_socket))
                .is_err()
            {
                return;
            }
            let writer_events = tx_events.clone();
            thread::spawn(move || {
                let mut writer = writer;
                if let Err(e) = write_client(
                    &mut writer,
                    &ClientMessage::Authenticate {
                        token,
                        clipboard,
                        profile,
                        phone_workspace: false,
                    },
                ) {
                    let _ = writer_events
                        .send(NetEvent::Closed(format!("Authentication send failed: {e}")));
                    return;
                }
                let mut last_ping = Instant::now();
                loop {
                    match out_rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(message) => {
                            if let Err(e) = write_client(&mut writer, &message) {
                                let _ = writer_events
                                    .send(NetEvent::Closed(format!("Send failed: {e}")));
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if last_ping.elapsed() >= Duration::from_secs(5) {
                        if let Err(e) = write_client(&mut writer, &ClientMessage::Ping) {
                            let _ = writer_events
                                .send(NetEvent::Closed(format!("Heartbeat failed: {e}")));
                            break;
                        }
                        last_ping = Instant::now();
                    }
                }
            });
            let _ = tx_events.send(NetEvent::Status("Authenticating…".into()));
            let mut decoder: Option<Decoder> = None;
            let mut first = true;
            let mut read = reader;
            loop {
                match read_server(&mut read) {
                    Ok(ServerMessage::Ready {
                        width,
                        height,
                        diagnostics,
                        profile,
                        ..
                    }) if first => {
                        first = false;
                        if width == 0 || height == 0 || width > 8192 || height > 8192 {
                            let _ = tx_events.send(NetEvent::Closed(
                                "Server declared invalid video dimensions".into(),
                            ));
                            return;
                        }
                        match Decoder::start(
                            width,
                            height,
                            frame_cache.clone(),
                            render_context.clone(),
                        ) {
                            Ok(d) => {
                                decoder = Some(d);
                                let _ = tx_events.send(NetEvent::Ready(
                                    width,
                                    height,
                                    diagnostics,
                                    profile,
                                ));
                            }
                            Err(e) => {
                                let _ = tx_events.send(NetEvent::Closed(e));
                                return;
                            }
                        }
                    }
                    Ok(ServerMessage::Ready { .. }) => {
                        let _ = tx_events
                            .send(NetEvent::Closed("Unexpected second Ready message".into()));
                        return;
                    }
                    Ok(ServerMessage::Error { message }) if first => {
                        let _ = tx_events.send(NetEvent::Closed(format!(
                            "Server rejected connection: {message}"
                        )));
                        return;
                    }
                    Ok(ServerMessage::Video(bytes)) if !first => {
                        video_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        if let Some(d) = decoder.as_mut() {
                            if let Err(e) = d.push(&bytes) {
                                let _ = tx_events.send(NetEvent::Closed(e));
                                return;
                            }
                        }
                    }
                    Ok(ServerMessage::Clipboard { text }) if !first => {
                        let _ = tx_events.send(NetEvent::Clipboard(text));
                    }
                    Ok(ServerMessage::ClipboardStatus { enabled }) if !first => {
                        let _ = tx_events.send(NetEvent::ClipboardStatus(enabled));
                    }
                    Ok(ServerMessage::InputStatus { enabled }) if !first => {
                        let _ = tx_events.send(NetEvent::InputStatus(enabled));
                    }
                    Ok(ServerMessage::Diagnostics { usage }) if !first => {
                        let _ = tx_events.send(NetEvent::Diagnostics(usage));
                    }
                    Ok(ServerMessage::Pong) => {}
                    Ok(ServerMessage::Error { message }) => {
                        let _ =
                            tx_events.send(NetEvent::Closed(format!("Server error: {message}")));
                        return;
                    }
                    Ok(_) => {
                        let _ =
                            tx_events.send(NetEvent::Closed("Unexpected server message".into()));
                        return;
                    }
                    Err(e) => {
                        let _ = tx_events.send(NetEvent::Closed(format!("Connection closed: {e}")));
                        return;
                    }
                }
            }
        });

        self.connected = true;
        self.status = "Connecting…".into();
        self.retry_at = None;
    }

    fn send(&mut self, message: ClientMessage) {
        if let Some(tx) = &self.tx {
            if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(message) {
                self.disconnect("The control connection could not keep up.".into());
            }
        }
    }

    fn key(&mut self, code: &'static str, pressed: bool) {
        let changed = if pressed {
            self.held_keys.insert(code)
        } else {
            self.held_keys.remove(code)
        };
        if changed {
            self.send(ClientMessage::Input(InputEvent::Key {
                code: code.into(),
                pressed,
            }));
        }
    }

    fn update_modifiers(&mut self, modifiers: Modifiers) {
        for (before, after, code) in [
            (self.last_modifiers.ctrl, modifiers.ctrl, "ControlLeft"),
            (self.last_modifiers.mac_cmd, modifiers.mac_cmd, "MetaLeft"),
            (self.last_modifiers.shift, modifiers.shift, "ShiftLeft"),
            (self.last_modifiers.alt, modifiers.alt, "AltLeft"),
        ] {
            if before != after {
                self.key(self.map_modifier(code), after);
            }
        }
        self.last_modifiers = modifiers;
    }

    fn map_modifier(&self, code: &'static str) -> &'static str {
        match (self.map_cmd_ctrl, code) {
            (true, "ControlLeft") => "MetaLeft",
            (true, "MetaLeft") => "ControlLeft",
            _ => code,
        }
    }

    fn clipboard_shortcut(&mut self, code: &'static str) {
        let native = if cfg!(target_os = "macos") {
            "MetaLeft"
        } else {
            "ControlLeft"
        };
        let modifier = self.map_modifier(native);
        let was_held = self.held_keys.contains(modifier);
        self.key(modifier, true);
        self.key(code, true);
        self.key(code, false);
        if !was_held {
            self.key(modifier, false);
        }
    }

    fn release_input(&mut self) {
        self.shortcut_keys.clear();
        let keys: Vec<_> = self.held_keys.iter().copied().collect();
        for code in keys {
            self.key(code, false);
        }
        for button in std::mem::take(&mut self.held_buttons) {
            self.send(ClientMessage::Input(InputEvent::PointerButton {
                button,
                pressed: false,
            }));
        }
        self.last_modifiers = Modifiers::default();
    }

    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    fn get_clipboard(&mut self) -> Option<String> {
        self.clipboard()?.get_text().ok()
    }

    fn send_clipboard(&mut self, text: String) {
        if self.clipboard_ready
            && text.len() <= remote_protocol::MAX_CLIPBOARD_TEXT
            && text != self.last_clipboard
        {
            self.last_clipboard = text.clone();
            self.send(ClientMessage::Clipboard { text });
        }
    }

    fn disconnect(&mut self, reason: String) {
        self.connected = false;
        self.network_events = None;
        if let Some(cancel) = self.cancel_connect.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        self.tx = None;
        if let Some(socket) = self.socket.take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.status = reason;
        self.dimensions = None;
        self.remote_diagnostics = false;
        self.diagnostics.reset_connection();
        self.clipboard_ready = false;
        self.held_keys.clear();
        self.shortcut_keys.clear();
        self.held_buttons.clear();
        self.last_modifiers = Modifiers::default();
        self.texture = None;
        self.startup_deadline = None;
    }

    fn connection_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            workspace::mark(ui, 31.0);
            ui.label(workspace::title("Mini Remote", 15.0));
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.label(workspace::title("Devices", 13.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.connected
                    && workspace::icon(ui, workspace::Icon::Plus, "New connection", false).clicked()
                {
                    self.retry_at = None;
                    self.host.clear();
                    self.port = remote_protocol::DEFAULT_PORT.to_string();
                    self.token.clear();
                    self.connection_report = None;
                    self.connection_check = None;
                    self.status = "Disconnected".into();
                }
            });
        });
        if self.recent.entries().is_empty() {
            ui.label(workspace::muted("Recent connections appear here."));
        }
        let mut selected = None;
        let mut forgotten = None;
        for (index, address) in self.recent.entries().iter().enumerate() {
            let current = self.host.eq_ignore_ascii_case(&address.host)
                && self.port == address.port.to_string();
            let active = self.connected && current;
            egui::Frame::none()
                .fill(if current {
                    workspace::SURFACE
                } else {
                    egui::Color32::TRANSPARENT
                })
                .rounding(8.0)
                .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let label = if address.port == remote_protocol::DEFAULT_PORT {
                            address.host.clone()
                        } else {
                            format!("{} · {}", address.host, address.port)
                        };
                        let row = ui
                            .add_sized(
                                [
                                    if active {
                                        ui.available_width()
                                    } else {
                                        (ui.available_width() - 36.0).max(48.0)
                                    },
                                    30.0,
                                ],
                                egui::Button::new(workspace::title(label, 13.0))
                                    .frame(false)
                                    .truncate()
                                    .sense(if self.connected {
                                        egui::Sense::hover()
                                    } else {
                                        egui::Sense::click()
                                    }),
                            )
                            .on_hover_text(&address.host);
                        if row.clicked() {
                            selected = Some(address.clone());
                        }
                        if !active
                            && workspace::icon(
                                ui,
                                workspace::Icon::Close,
                                &format!("Forget {}", address.host),
                                false,
                            )
                            .clicked()
                        {
                            forgotten = Some(index);
                        }
                    });
                    if active {
                        workspace::status(ui, "Live", true);
                    }
                });
        }
        if let Some(index) = forgotten {
            self.recent.forget(index);
        }
        if let Some(address) = selected {
            self.retry_at = None;
            if !self.host.eq_ignore_ascii_case(&address.host)
                || self.port != address.port.to_string()
            {
                self.token.clear();
            }
            self.host = address.host;
            self.port = address.port.to_string();
            self.connection_report = None;
            self.connection_check = None;
            self.status = "Disconnected".into();
        }
    }

    fn run_connection_test(&mut self) {
        if self.connected || self.host.trim().is_empty() || self.connection_check.is_some() {
            return;
        }
        self.retry_at = None;
        let Some(port) = self.port.parse::<u16>().ok().filter(|port| *port > 0) else {
            self.connection_report = Some(Err("Enter a port from 1 to 65535.".into()));
            return;
        };
        let host = self.host.trim().to_owned();
        let token = self.token.clone();
        let profile = self.stream_profile;
        let (sender, receiver) = mpsc::channel();
        self.connection_report = None;
        self.connection_check = Some(receiver);
        thread::spawn(move || {
            let _ = sender.send(connection_test::run(&host, port, &token, profile));
        });
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        use workspace::Icon;

        egui::TopBottomPanel::top("toolbar")
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(workspace::SHELL)
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
            )
            .show(ctx, |ui| {
                let compact = ui.available_width() < 760.0;
                ui.horizontal(|ui| {
                    if workspace::icon(
                        ui,
                        Icon::Sidebar,
                        if self.sidebar {
                            "Hide devices"
                        } else {
                            "Show devices"
                        },
                        self.sidebar,
                    )
                    .clicked()
                    {
                        self.sidebar = !self.sidebar;
                    }
                    if !compact {
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.set_max_width((ui.available_width() - 570.0).clamp(105.0, 286.0));
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.add(
                                egui::Label::new(workspace::title(
                                    if self.connected {
                                        self.host.as_str()
                                    } else {
                                        "Workspace"
                                    },
                                    15.0,
                                ))
                                .truncate(),
                            );
                            if self.connected {
                                workspace::status(ui, &self.status, self.dimensions.is_some());
                            }
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(workspace::nav("Setup", false)).clicked() {
                            self.release_input();
                            self.setup = Some(setup::Guide::default());
                        }
                        if ui.add(workspace::nav("Shortcuts", false)).clicked() {
                            self.release_input();
                            self.shortcut_draft = Some(self.shortcuts);
                        }
                        if ui
                            .add(workspace::nav("Diagnostics", self.diagnostics.open))
                            .clicked()
                        {
                            self.diagnostics.toggle();
                        }
                        egui::menu::menu_custom_button(ui, workspace::nav("View", false), |ui| {
                            if ui
                                .button(if self.sidebar {
                                    "Hide devices"
                                } else {
                                    "Show devices"
                                })
                                .clicked()
                            {
                                self.sidebar = !self.sidebar;
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(
                                    self.connected,
                                    egui::Button::new(if self.full_screen {
                                        "Exit full screen"
                                    } else {
                                        "Full screen"
                                    }),
                                )
                                .clicked()
                            {
                                self.full_screen = !self.full_screen;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                                    self.full_screen,
                                ));
                                ui.close_menu();
                            }
                            if ui
                                .button(if self.diagnostics.open {
                                    "Hide diagnostics"
                                } else {
                                    "Show diagnostics"
                                })
                                .clicked()
                            {
                                self.diagnostics.toggle();
                                ui.close_menu();
                            }
                        });
                        egui::menu::menu_custom_button(
                            ui,
                            workspace::nav("Connection", false),
                            |ui| {
                                if self.connected {
                                    self.session_menu(ui);
                                } else {
                                    let can_test = self.connection_check.is_none()
                                        && !self.host.trim().is_empty();
                                    if ui
                                        .add_enabled(can_test, egui::Button::new("Test connection"))
                                        .clicked()
                                    {
                                        self.run_connection_test();
                                        ui.close_menu();
                                    }
                                    if self.retry_at.is_some()
                                        && ui.button("Cancel reconnect").clicked()
                                    {
                                        self.retry_at = None;
                                        self.retry_count = 0;
                                        self.status = "Disconnected".into();
                                    }
                                }
                            },
                        );
                    });
                });
            });
    }

    fn session_menu(&mut self, ui: &mut egui::Ui) {
        let mut clipboard = self.clipboard_enabled;
        if ui
            .add_enabled(
                !self.clipboard_denied,
                egui::Checkbox::new(&mut clipboard, "Two-way clipboard"),
            )
            .changed()
        {
            self.clipboard_enabled = clipboard;
            self.clipboard_ready = false;
            if clipboard {
                self.last_clipboard = self.get_clipboard().unwrap_or_default();
            }
            self.send(ClientMessage::ClipboardEnabled { enabled: clipboard });
        }
        if self.input_enabled {
            ui.separator();
            for (index, (_, shortcut)) in shortcuts::ACTIONS.iter().copied().enumerate() {
                if ui.button(self.shortcuts.label(index)).clicked() {
                    self.send(ClientMessage::WindowShortcut { shortcut });
                    ui.close_menu();
                }
            }
        }
        ui.separator();
        if ui.button("Disconnect").clicked() {
            self.disconnect("Disconnected".into());
            ui.close_menu();
        }
    }

    fn connection_form(&mut self, ui: &mut egui::Ui) {
        let width = (ui.available_width() - 24.0).clamp(220.0, 440.0);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.14).clamp(22.0, 86.0));
            ui.allocate_ui_with_layout(
                egui::vec2(width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width);
                    workspace::mark(ui, 42.0);
                    ui.add_space(6.0);
                    ui.label(workspace::title("Connect to a device", 27.0));
                    ui.label(workspace::muted(
                        "Enter its Tailscale address and pairing token.",
                    ));
                    ui.add_space(12.0);

                    workspace::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(workspace::title("Host", 12.0));
                        let changed_host = ui
                            .add(
                                workspace::field(&mut self.host)
                                    .hint_text("Tailscale name or IP address")
                                    .desired_width(f32::INFINITY),
                            )
                            .changed();
                        ui.add_space(4.0);
                        ui.label(workspace::title("Pairing token", 12.0));
                        if changed_host {
                            self.connection_report = None;
                            self.connection_check = None;
                            self.token.clear();
                            self.retry_at = None;
                        }
                        let token = ui.add(
                            workspace::field(&mut self.token)
                                .password(true)
                                .hint_text("Paste the token from the connector")
                                .desired_width(f32::INFINITY),
                        );
                        if token.changed() {
                            self.connection_report = None;
                            self.connection_check = None;
                        }
                        let submit =
                            token.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));

                        ui.add_space(4.0);
                        egui::CollapsingHeader::new("Connection options").show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Port");
                                if ui
                                    .add(workspace::field(&mut self.port).desired_width(68.0))
                                    .changed()
                                {
                                    self.connection_report = None;
                                    self.connection_check = None;
                                    self.token.clear();
                                    self.retry_at = None;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Stream quality");
                                let previous = self.stream_profile;
                                egui::ComboBox::from_id_salt("stream-profile")
                                    .selected_text(quality_label(previous))
                                    .show_ui(ui, |ui| {
                                        for profile in [
                                            StreamProfile::Desktop,
                                            StreamProfile::LowData,
                                            StreamProfile::Balanced,
                                            StreamProfile::Sharp,
                                        ] {
                                            ui.selectable_value(
                                                &mut self.stream_profile,
                                                profile,
                                                quality_label(profile),
                                            );
                                        }
                                    });
                                if previous != self.stream_profile {
                                    self.connection_report = None;
                                    self.connection_check = None;
                                }
                            });
                            ui.checkbox(&mut self.clipboard_enabled, "Enable two-way clipboard");
                            ui.checkbox(&mut self.map_cmd_ctrl, "Map Command ↔ Control");
                            ui.checkbox(&mut self.auto_reconnect, "Reconnect automatically");
                        });
                        if self.status != "Disconnected" {
                            ui.label(workspace::muted(&self.status));
                        }
                        if let Some(report) = &self.connection_report {
                            match report {
                                Ok(message) => {
                                    ui.label(egui::RichText::new(message).color(workspace::ACCENT));
                                }
                                Err(message) => {
                                    ui.colored_label(ui.visuals().error_fg_color, message);
                                }
                            }
                        }
                        if self.connection_check.is_some() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(workspace::muted("Testing connection…"));
                            });
                        }
                        ui.add_space(7.0);
                        let available = self.connection_check.is_none();
                        let has_host = !self.host.trim().is_empty();
                        let can_connect = available && has_host && !self.token.trim().is_empty();
                        ui.horizontal_wrapped(|ui| {
                            if (ui
                                .add_enabled(can_connect, workspace::primary("Connect"))
                                .clicked()
                                || submit)
                                && can_connect
                            {
                                self.connect(ui.ctx());
                            }
                            if ui
                                .add_enabled(
                                    available && has_host,
                                    workspace::secondary("Test connection"),
                                )
                                .clicked()
                            {
                                self.run_connection_test();
                            }
                            #[cfg(target_os = "linux")]
                            if ui
                                .add_enabled(
                                    available && has_host && !self.mac_signin_active,
                                    workspace::secondary("Mac sign-in"),
                                )
                                .on_hover_text("Open Apple Screen Sharing in TigerVNC")
                                .clicked()
                            {
                                match mac_signin::launch(
                                    self.host.trim().to_owned(),
                                    self.helper_sender.clone(),
                                ) {
                                    Ok(()) => {
                                        self.mac_signin_active = true;
                                        self.status = "Opening Mac sign-in…".into();
                                    }
                                    Err(message) => self.status = message,
                                }
                            }
                        });
                    });
                },
            );
        });
    }

    fn next_event(&self) -> Option<NetEvent> {
        let event = self
            .network_events
            .as_ref()
            .and_then(|queue| queue.try_recv().ok());
        #[cfg(target_os = "linux")]
        let event = event.or_else(|| self.helper_events.try_recv().ok());
        event
    }

    fn receive_events(&mut self) {
        while let Some(event) = self.next_event() {
            match event {
                NetEvent::Status(s) => self.status = s,
                NetEvent::Connected(tx, socket) => {
                    if self.connected {
                        self.tx = Some(tx);
                        self.socket = Some(socket);
                    } else {
                        let _ = socket.shutdown(Shutdown::Both);
                    }
                }
                NetEvent::Ready(w, h, supports_diagnostics, profile) => {
                    self.dimensions = Some((w, h));
                    self.remote_diagnostics = supports_diagnostics;
                    self.diagnostics.reset_connection();
                    if let Ok(port) = self.port.parse() {
                        self.recent.remember(&self.host, port);
                    }
                    self.status = if profile == StreamProfile::Desktop {
                        format!("Connected · {w} × {h}")
                    } else {
                        format!("{} · {w} × {h}", quality_label(profile))
                    };
                    self.connected = true;
                    self.retry_count = 0;
                    self.retry_at = None;
                    self.startup_deadline = Some(Instant::now() + Duration::from_secs(8));
                }
                NetEvent::Diagnostics(usage) => self.diagnostics.remote_sample(usage),
                NetEvent::ClipboardStatus(enabled) => {
                    self.clipboard_ready = enabled && self.clipboard_enabled;
                    self.clipboard_denied = !enabled && self.clipboard_enabled;
                    if self.clipboard_denied {
                        self.clipboard_enabled = false;
                        self.status =
                            "Connected · clipboard sharing is disabled by the host".into();
                    }
                }
                NetEvent::InputStatus(enabled) => {
                    self.input_enabled = enabled;
                    if !enabled {
                        self.status = if self.clipboard_denied {
                            "Connected · host disabled clipboard sharing and needs Accessibility permission for keyboard and mouse".into()
                        } else {
                            "Connected · host needs Accessibility permission for keyboard and mouse"
                                .into()
                        };
                    }
                }
                #[cfg(target_os = "linux")]
                NetEvent::MacSignIn { message, active } => {
                    self.status = message;
                    self.mac_signin_active = active;
                }
                NetEvent::Clipboard(text) if self.clipboard_ready => {
                    if text != self.last_clipboard
                        && self
                            .clipboard()
                            .is_some_and(|clipboard| clipboard.set_text(text.clone()).is_ok())
                    {
                        self.last_clipboard = text;
                    }
                }
                NetEvent::Clipboard(_) => {}
                NetEvent::Closed(s) => {
                    let was_connected = self.connected;
                    self.disconnect(s.clone());
                    let retryable = !(s.starts_with("Server rejected")
                        || s.starts_with("Server error")
                        || s.starts_with("Unexpected")
                        || s.starts_with("Could not start FFmpeg")
                        || s.starts_with("Server declared"));
                    if was_connected && retryable && self.auto_reconnect && self.retry_count < 5 {
                        self.retry_count += 1;
                        let delay = 1_u64 << (self.retry_count - 1);
                        self.retry_at = Some(Instant::now() + Duration::from_secs(delay));
                        self.status =
                            format!("{s} · reconnecting in {delay}s ({}/5)", self.retry_count);
                    }
                }
            }
        }
    }

    fn render_latest_frame(&mut self, ctx: &egui::Context) {
        let frame = self
            .frame_cache
            .lock()
            .ok()
            .and_then(|mut buffer| buffer.latest.take());
        let Some(frame) = frame.filter(|_| self.connected && self.dimensions.is_some()) else {
            return;
        };
        self.displayed_frames += 1;
        let image = ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("desktop-frame", image, egui::TextureOptions::LINEAR));
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "Mini Remote Viewer · {} × {}",
                frame.width, frame.height
            )));
            eprintln!(
                "Displaying a {} × {} remote desktop.",
                frame.width, frame.height
            );
        }
        if let Ok(mut buffer) = self.frame_cache.lock() {
            buffer.reusable = Some(frame.rgb);
        }
        self.startup_deadline = None;
    }

    fn poll_clipboard(&mut self) {
        if self.clipboard_ready && self.last_clipboard_poll.elapsed() >= Duration::from_millis(250)
        {
            self.last_clipboard_poll = Instant::now();
            if let Some(text) = self.get_clipboard() {
                self.send_clipboard(text);
            }
        }
    }
}

impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.receive_events();
        self.render_latest_frame(ctx);
        self.poll_clipboard();
        if self.diagnostics.tick(
            self.video_bytes.load(Ordering::Relaxed),
            self.displayed_frames,
        ) && self.remote_diagnostics
        {
            self.send(ClientMessage::RequestDiagnostics);
        }
        if self.retry_at.is_some_and(|when| Instant::now() >= when) {
            self.retry_at = None;
            self.connect(ctx);
        }
        if self
            .startup_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
            && self.texture.is_none()
        {
            self.disconnect("Connected, but no video frames arrived within 8 seconds. Check the host capture/encoder.".into());
        }

        if self.connect_on_start {
            self.connect_on_start = false;
            self.connect(ctx);
        }
        if let Some(check) = &self.connection_check {
            match check.try_recv() {
                Ok(result) => {
                    self.connection_report = Some(result);
                    self.connection_check = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.connection_report =
                        Some(Err("The connection test did not finish.".into()));
                    self.connection_check = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(guide) = &mut self.setup {
            let close = egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(workspace::CANVAS))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .show(ui, |ui| guide.show(ui))
                        .inner
                })
                .inner;
            if close {
                self.setup_seen = true;
                self.setup = None;
            }
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if let Some(draft) = &mut self.shortcut_draft {
            let outcome = egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(workspace::CANVAS))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .show(ui, |ui| draft.edit(ui, &mut self.shortcut_recording))
                        .inner
                })
                .inner;
            if let Some(save) = outcome {
                if save {
                    self.shortcuts = *draft;
                    if let Some(storage) = frame.storage_mut() {
                        eframe::set_value(
                            storage,
                            "remote-desktop-viewer.setup-seen",
                            &self.setup_seen,
                        );
                        self.shortcuts.save(storage);
                        storage.flush();
                    }
                }
                self.shortcut_draft = None;
                self.shortcut_recording = None;
            }
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        if self.connected && self.dimensions.is_some() && self.input_enabled {
            let events = ctx.input(|i| i.events.clone());
            for event in events {
                match event {
                    egui::Event::PointerMoved(pos) => {
                        if let Some(rect) = self.image_rect.filter(|r| r.contains(pos)) {
                            let x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            let y = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                            self.send(ClientMessage::Input(InputEvent::PointerMove { x, y }));
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        ..
                    } => {
                        let button = match button {
                            egui::PointerButton::Primary => MouseButton::Left,
                            egui::PointerButton::Middle => MouseButton::Middle,
                            egui::PointerButton::Secondary => MouseButton::Right,
                            egui::PointerButton::Extra1 => MouseButton::Back,
                            egui::PointerButton::Extra2 => MouseButton::Forward,
                        };
                        let changed = if pressed {
                            self.image_rect.is_some_and(|rect| rect.contains(pos))
                                && self.held_buttons.insert(button)
                        } else {
                            self.held_buttons.remove(&button)
                        };
                        if changed {
                            self.send(ClientMessage::Input(InputEvent::PointerButton {
                                button,
                                pressed,
                            }));
                        }
                    }
                    egui::Event::MouseWheel { delta, unit, .. } => {
                        if ctx
                            .input(|i| i.pointer.hover_pos())
                            .is_some_and(|pos| self.image_rect.is_some_and(|r| r.contains(pos)))
                        {
                            let scale = match unit {
                                egui::MouseWheelUnit::Point => 1.0 / 40.0,
                                egui::MouseWheelUnit::Line => 1.0,
                                egui::MouseWheelUnit::Page => 12.0,
                            };
                            self.send(ClientMessage::Input(InputEvent::Scroll {
                                x: (-delta.x * scale).clamp(-100.0, 100.0),
                                y: (delta.y * scale).clamp(-100.0, 100.0),
                            }));
                        }
                    }
                    egui::Event::Key {
                        physical_key: Some(key),
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } => {
                        if !pressed && self.shortcut_keys.remove(&key) {
                            continue;
                        }
                        if repeat {
                            continue;
                        }
                        if pressed {
                            if let Some(shortcut) = self.shortcuts.action(key, modifiers) {
                                self.shortcut_keys.insert(key);
                                self.send(ClientMessage::WindowShortcut { shortcut });
                                continue;
                            }
                        }
                        self.update_modifiers(modifiers);
                        if let Some(code) = key_code(key) {
                            self.key(code, pressed);
                        }
                    }
                    egui::Event::Copy => self.clipboard_shortcut("KeyC"),
                    egui::Event::Cut => self.clipboard_shortcut("KeyX"),
                    egui::Event::Paste(text) => {
                        if self.clipboard_ready {
                            let text = self.get_clipboard().unwrap_or(text);
                            self.send_clipboard(text);
                        }
                        self.clipboard_shortcut("KeyV");
                    }
                    egui::Event::WindowFocused(false) => self.release_input(),
                    _ => {}
                }
            }
            if ctx.input(|input| input.focused) {
                self.update_modifiers(ctx.input(|input| input.modifiers));
            }
        }

        self.toolbar(ctx);

        if self.sidebar {
            egui::SidePanel::left("connections")
                .show_separator_line(false)
                .default_width(220.0)
                .width_range(170.0..=270.0)
                .frame(
                    egui::Frame::none()
                        .fill(workspace::SIDEBAR)
                        .inner_margin(14.0),
                )
                .show(ctx, |ui| self.connection_sidebar(ui));
        }
        if self.diagnostics.open {
            egui::SidePanel::right("diagnostics")
                .show_separator_line(false)
                .default_width(300.0)
                .width_range(260.0..=380.0)
                .frame(
                    egui::Frame::none()
                        .fill(workspace::SIDEBAR)
                        .inner_margin(14.0),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.diagnostics
                            .show(ui, self.dimensions, self.remote_diagnostics)
                    });
                });
        }

        if !self.connected {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(workspace::CANVAS)
                        .inner_margin(egui::Margin::same(16.0)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.connection_form(ui));
                });
        } else {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(workspace::CANVAS)
                        .inner_margin(egui::Margin::same(10.0)),
                )
                .show(ctx, |ui| {
                    let available = ui.available_size();
                    if let (Some((width, height)), Some(texture)) = (self.dimensions, &self.texture)
                    {
                        let size = Vec2::new(width as f32, height as f32);
                        let scale = (available.x / size.x).min(available.y / size.y).max(0.01);
                        let display = size * scale;
                        let rect = egui::Rect::from_center_size(
                            ui.available_rect_before_wrap().center(),
                            display,
                        );
                        ui.allocate_rect(rect, egui::Sense::click_and_drag());
                        self.image_rect = Some(rect);
                        ui.painter().rect_stroke(
                            rect.expand(1.0),
                            1.0,
                            egui::Stroke::new(1.0_f32, workspace::BORDER),
                        );
                        ui.painter().image(
                            texture.id(),
                            rect,
                            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    } else {
                        self.image_rect = None;
                        ui.centered_and_justified(|ui| ui.spinner());
                    }
                });
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("remote-desktop-viewer.host", self.host.clone());
        storage.set_string("remote-desktop-viewer.port", self.port.clone());
        self.shortcuts.save(storage);
        self.recent.save(storage);
        eframe::set_value(
            storage,
            "remote-desktop-viewer.stream-quality",
            &self.stream_profile,
        );
    }
}

fn quality_label(profile: StreamProfile) -> &'static str {
    match profile {
        StreamProfile::Desktop => "Native resolution",
        StreamProfile::LowData => "Low data",
        StreamProfile::Balanced => "Balanced",
        StreamProfile::Sharp => "Sharp",
    }
}

fn key_code(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::A => "KeyA",
        Key::B => "KeyB",
        Key::C => "KeyC",
        Key::D => "KeyD",
        Key::E => "KeyE",
        Key::F => "KeyF",
        Key::G => "KeyG",
        Key::H => "KeyH",
        Key::I => "KeyI",
        Key::J => "KeyJ",
        Key::K => "KeyK",
        Key::L => "KeyL",
        Key::M => "KeyM",
        Key::N => "KeyN",
        Key::O => "KeyO",
        Key::P => "KeyP",
        Key::Q => "KeyQ",
        Key::R => "KeyR",
        Key::S => "KeyS",
        Key::T => "KeyT",
        Key::U => "KeyU",
        Key::V => "KeyV",
        Key::W => "KeyW",
        Key::X => "KeyX",
        Key::Y => "KeyY",
        Key::Z => "KeyZ",
        Key::Num0 => "Digit0",
        Key::Num1 => "Digit1",
        Key::Num2 => "Digit2",
        Key::Num3 => "Digit3",
        Key::Num4 => "Digit4",
        Key::Num5 => "Digit5",
        Key::Num6 => "Digit6",
        Key::Num7 => "Digit7",
        Key::Num8 => "Digit8",
        Key::Num9 => "Digit9",
        Key::Escape => "Escape",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Enter => "Enter",
        Key::Space => "Space",
        Key::Insert => "Insert",
        Key::Delete => "Delete",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::F13 => "F13",
        Key::F14 => "F14",
        Key::F15 => "F15",
        Key::F16 => "F16",
        Key::F17 => "F17",
        Key::F18 => "F18",
        Key::F19 => "F19",
        Key::F20 => "F20",
        Key::F21 => "F21",
        Key::F22 => "F22",
        Key::F23 => "F23",
        Key::F24 => "F24",
        Key::F25 => "F25",
        Key::F26 => "F26",
        Key::F27 => "F27",
        Key::F28 => "F28",
        Key::F29 => "F29",
        Key::F30 => "F30",
        Key::F31 => "F31",
        Key::F32 => "F32",
        Key::F33 => "F33",
        Key::F34 => "F34",
        Key::F35 => "F35",
        Key::Comma => "Comma",
        Key::Backslash => "Backslash",
        Key::Slash => "Slash",
        Key::OpenBracket => "BracketLeft",
        Key::CloseBracket => "BracketRight",
        Key::Backtick => "Backquote",
        Key::Minus => "Minus",
        Key::Period => "Period",
        Key::Plus | Key::Equals => "Equal",
        Key::Semicolon | Key::Colon => "Semicolon",
        Key::Quote => "Quote",
        Key::Pipe => "Backslash",
        Key::Questionmark => "Slash",
        _ => return None,
    })
}

struct Decoder {
    child: Child,
    stdin: ChildStdin,
    diagnostics: Arc<Mutex<String>>,
}

impl Decoder {
    fn start(
        width: u32,
        height: u32,
        frame_cache: Arc<Mutex<Frames>>,
        render_context: egui::Context,
    ) -> Result<Self, String> {
        let ffmpeg = ffmpeg_executable();
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-f", "h264"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "macos")]
        {
            cmd.args(["-hwaccel", "videotoolbox"]);
        }
        #[cfg(target_os = "linux")]
        if let Some(device) = linux_video_device(&ffmpeg) {
            cmd.args(["-hwaccel", "vaapi", "-hwaccel_device"])
                .arg(device);
        } else {
            cmd.args(["-hwaccel", "auto"]);
        }
        cmd.args([
            "-threads",
            "2",
            "-filter_threads",
            "1",
            "-probesize",
            "65536",
            "-analyzeduration",
            "100000",
            "-flags",
            "low_delay",
            "-i",
            "pipe:0",
            "-an",
            "-fps_mode",
            "passthrough",
            "-threads:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ]);
        let mut child = cmd.spawn().map_err(|e| {
            format!("Could not start FFmpeg decoder ({e}). Install FFmpeg and retry.")
        })?;
        let stdin = child.stdin.take().ok_or("FFmpeg stdin unavailable")?;
        let mut stdout = child.stdout.take().ok_or("FFmpeg stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("FFmpeg stderr unavailable")?;
        let diagnostics = Arc::new(Mutex::new(String::new()));
        let diagnostics_reader = diagnostics.clone();
        thread::spawn(move || {
            let mut lines = std::io::BufReader::new(stderr).lines();
            while let Some(Ok(line)) = lines.next() {
                if let Ok(mut text) = diagnostics_reader.lock() {
                    if text.len() + line.len() > 2048 {
                        text.clear();
                    }
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        });
        let w = width as usize;
        let h = height as usize;
        thread::spawn(move || {
            let frame_size = w.saturating_mul(h).saturating_mul(3);
            if frame_size == 0 || frame_size > 256 * 1024 * 1024 {
                return;
            }
            let mut buf = vec![0; frame_size];
            loop {
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                if let Ok(mut slot) = frame_cache.lock() {
                    let spare = slot
                        .latest
                        .take()
                        .map(|frame| frame.rgb)
                        .or_else(|| slot.reusable.take())
                        .unwrap_or_else(|| vec![0; frame_size]);
                    slot.latest = Some(Frame {
                        width: w,
                        height: h,
                        rgb: std::mem::replace(&mut buf, spare),
                    });
                    render_context.request_repaint();
                } else {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            diagnostics,
        })
    }
    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|e| format!("Could not inspect FFmpeg decoder: {e}"))?
        {
            let detail = self
                .diagnostics
                .lock()
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "no decoder diagnostic was provided".into());
            return Err(format!("FFmpeg video decoder exited ({status}): {detail}"));
        }
        self.stdin.write_all(bytes).map_err(|e| {
            let detail = self
                .diagnostics
                .lock()
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| e.to_string());
            format!("FFmpeg video decoder stopped: {detail}")
        })
    }
}

#[cfg(target_os = "linux")]
fn linux_video_device(ffmpeg: &std::path::Path) -> Option<PathBuf> {
    let nodes = std::fs::read_dir("/sys/class/drm").ok()?;
    for node in nodes.flatten() {
        let name = node.file_name();
        if !name.to_string_lossy().starts_with("renderD") {
            continue;
        }
        let vendor = std::fs::read_to_string(node.path().join("device/vendor")).unwrap_or_default();
        if !matches!(vendor.trim(), "0x1002" | "0x8086") {
            continue;
        }
        let device = PathBuf::from("/dev/dri").join(name);
        let success = Command::new(ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-init_hw_device",
            ])
            .arg(format!("vaapi:{}", device.display()))
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=size=16x16",
                "-frames:v",
                "1",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if success {
            return Some(device);
        }
    }
    None
}

fn ffmpeg_executable() -> PathBuf {
    if let Some(path) = std::env::var_os("MINIREMOTE_FFMPEG") {
        return path.into();
    }
    #[cfg(target_os = "macos")]
    for path in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/ffmpeg");
        if candidate.is_file() {
            return candidate;
        }
    }
    "ffmpeg".into()
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn main() -> eframe::Result<()> {
    let Some(config) = (match cli_defaults() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }) else {
        return Ok(());
    };
    if config.test_connection {
        let port = config.port.as_deref().unwrap_or("47990");
        let Some(port) = port.parse::<u16>().ok().filter(|port| *port > 0) else {
            eprintln!("Enter a port from 1 to 65535.");
            std::process::exit(2);
        };
        match connection_test::run(
            config.host.trim(),
            port,
            config.token.as_deref().unwrap_or(""),
            config.quality.unwrap_or_default(),
        ) {
            Ok(message) => println!("{message}"),
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Mini Remote")
        .with_inner_size([1100.0, 760.0])
        .with_min_inner_size([700.0, 500.0]);
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/mini-remote-128.png"))
    {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Remote Desktop Viewer",
        options,
        Box::new(move |cc| {
            workspace::appearance(&cc.egui_ctx);
            let mut viewer = Viewer::default();
            if let Some(storage) = cc.storage {
                viewer.shortcuts = shortcuts::Bindings::load(storage);
                viewer.recent = workspace::RecentHosts::load(storage);
                viewer.setup_seen =
                    eframe::get_value(storage, "remote-desktop-viewer.setup-seen").unwrap_or(false);
                viewer.stream_profile =
                    eframe::get_value(storage, "remote-desktop-viewer.stream-quality")
                        .unwrap_or_default();
            }
            let saved_host = cc
                .storage
                .and_then(|s| s.get_string("remote-desktop-viewer.host"))
                .unwrap_or_default();
            let saved_port = cc
                .storage
                .and_then(|s| s.get_string("remote-desktop-viewer.port"))
                .unwrap_or_else(|| remote_protocol::DEFAULT_PORT.to_string());
            viewer.host = if config.host.is_empty() {
                saved_host
            } else {
                config.host.clone()
            };
            viewer.port = config.port.clone().unwrap_or(saved_port);
            viewer.token = config.token.clone().unwrap_or_default();
            viewer.connect_on_start = config.connect;
            if config.pair_phone {
                viewer.setup = Some(setup::Guide::pair_phone());
            } else if config.setup
                || (!viewer.setup_seen && viewer.host.is_empty() && !config.connect)
            {
                viewer.setup = Some(setup::Guide::default());
            }
            if let Some(quality) = config.quality {
                viewer.stream_profile = quality;
            }
            Ok(Box::new(viewer))
        }),
    )
}

struct CliOptions {
    host: String,
    port: Option<String>,
    token: Option<String>,
    connect: bool,
    quality: Option<StreamProfile>,
    test_connection: bool,
    setup: bool,
    pair_phone: bool,
}

fn cli_defaults() -> Result<Option<CliOptions>, String> {
    let mut host = String::new();
    let mut port = None;
    let mut token_file = None;
    let mut connect = false;
    let mut quality = None;
    let mut test_connection = false;
    let mut setup = false;
    let mut pair_phone = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--host" | "-H" => host = args.next().ok_or("--host requires an address")?,
            "--port" | "-p" => port = Some(args.next().ok_or("--port requires a number")?),
            "--token-file" => {
                token_file = Some(std::path::PathBuf::from(
                    args.next().ok_or("--token-file requires a path")?,
                ))
            }
            "--quality" => {
                quality = Some(match args.next().as_deref() {
                    Some("native") => StreamProfile::Desktop,
                    Some("low-data") => StreamProfile::LowData,
                    Some("balanced") => StreamProfile::Balanced,
                    Some("sharp") => StreamProfile::Sharp,
                    _ => {
                        return Err(
                            "--quality requires native, low-data, balanced, or sharp.".into()
                        )
                    }
                });
            }
            "--connect" => connect = true,
            "--test-connection" => test_connection = true,
            "--setup" => setup = true,
            "--pair-phone" => pair_phone = true,
            "--help" | "-h" => {
                println!("Usage: remote-desktop-viewer [--host TAILSCALE_NAME_OR_IP] [--port PORT] [--token-file PATH] [--quality PROFILE] [--connect | --test-connection] [--setup | --pair-phone]\n\n--connect starts a session at launch and requires --token-file. The token file must be private (0600 on Unix); its contents are only kept in memory. Tokens are never accepted directly as an argument or environment variable. Host and port bookmarks are saved by the app; credentials are not.\n--test-connection checks reachability, and pairing and desktop video if a token file is supplied.");
                return Ok(None);
            }
            _ => return Err(format!("Unknown argument: {arg}\nUse --help for usage.")),
        }
    }
    if test_connection && (connect || host.trim().is_empty()) {
        return Err(
            "--test-connection requires --host and cannot be combined with --connect".into(),
        );
    }
    if connect && token_file.is_none() {
        return Err("--connect requires --token-file PATH".into());
    }
    let token = if let Some(path) = token_file {
        let metadata =
            std::fs::metadata(&path).map_err(|e| format!("Cannot inspect token file: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(
                    "Token file must not be accessible to group or other users (chmod 600).".into(),
                );
            }
        }
        let token =
            std::fs::read_to_string(path).map_err(|e| format!("Cannot read token file: {e}"))?;
        let token = token.trim().to_owned();
        if token.is_empty() || token.len() > 128 {
            return Err("Token file must contain a token of 1 to 128 bytes.".into());
        }
        Some(token)
    } else {
        None
    };
    Ok(Some(CliOptions {
        host,
        port,
        token,
        connect,
        quality,
        test_connection,
        setup,
        pair_phone,
    }))
}
