//! Linux host backend for an authorized, already-running Wayland desktop.
//!
//! Capture and clipboard access are delegated to the compositor-aware `grim`,
//! `wl-copy`, and `wl-paste` clients. Capture is a bounded-memory pipeline: no
//! frames are written to disk, and the returned reader owns the encoder process.

use remote_protocol::{InputEvent, MAX_CLIPBOARD_TEXT, MouseButton, StreamProfile, VideoSource};
use std::{
    collections::HashMap,
    error::Error,
    io::{self, Read, Seek, SeekFrom, Write},
    os::fd::AsFd,
    process::{Child, Command, Stdio},
    time::Instant,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    protocol::{wl_output, wl_pointer, wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const FALLBACK_WIDTH: u32 = 1920;
const FALLBACK_HEIGHT: u32 = 1080;
const FPS: u32 = 30;
const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;

type HostError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Debug)]
struct OutputInfo {
    name: Option<String>,
    width: u32,
    height: u32,
    scale: f64,
    logical_width: u32,
    logical_height: u32,
}

fn read_output_info(display: &str, runtime_dir: &str) -> Result<OutputInfo, HostError> {
    let requested = std::env::var("REMOTE_DESKTOP_OUTPUT").ok();
    let result = Command::new("wlr-randr")
        .arg("--json")
        .env("WAYLAND_DISPLAY", display)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .output();
    if let Ok(output) = result {
        if output.status.success() {
            if let Some(info) = parse_output_info(&output.stdout, requested.as_deref())? {
                return Ok(info);
            }
            if requested.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "REMOTE_DESKTOP_OUTPUT does not name an enabled output",
                )
                .into());
            }
        }
    }

    let (width, height) = bounded_dimensions(FALLBACK_WIDTH, FALLBACK_HEIGHT);
    Ok(OutputInfo {
        name: requested,
        width,
        height,
        scale: 1.0,
        logical_width: FALLBACK_WIDTH,
        logical_height: FALLBACK_HEIGHT,
    })
}

fn parse_output_info(
    json: &[u8],
    requested: Option<&str>,
) -> Result<Option<OutputInfo>, HostError> {
    let outputs: serde_json::Value = serde_json::from_slice(json)?;
    let list = outputs.as_array().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "wlr-randr JSON must be an array",
        )
    })?;
    let selected = list.iter().find(|entry| {
        entry["enabled"].as_bool() == Some(true)
            && requested.is_none_or(|name| entry["name"].as_str() == Some(name))
    });
    let Some(output) = selected else {
        return Ok(None);
    };
    let mode = output["modes"].as_array().and_then(|modes| {
        modes
            .iter()
            .find(|mode| mode["current"].as_bool() == Some(true))
            .or_else(|| {
                modes
                    .iter()
                    .find(|mode| mode["preferred"].as_bool() == Some(true))
            })
            .or_else(|| modes.first())
    });
    let Some((name, mode)) = output["name"].as_str().zip(mode) else {
        return Ok(None);
    };
    let mut native_width = mode["width"].as_u64().unwrap_or(0) as u32;
    let mut native_height = mode["height"].as_u64().unwrap_or(0) as u32;
    let scale = output["scale"]
        .as_f64()
        .filter(|scale| scale.is_finite() && *scale > 0.0)
        .unwrap_or(1.0);
    let transform = output["transform"].as_str().unwrap_or("normal");
    if matches!(transform, "90" | "270" | "flipped-90" | "flipped-270") {
        std::mem::swap(&mut native_width, &mut native_height);
    }
    if native_width == 0 || native_height == 0 {
        return Ok(None);
    }
    let (width, height) = bounded_dimensions(native_width, native_height);
    Ok(Some(OutputInfo {
        name: Some(name.to_owned()),
        width,
        height,
        scale,
        logical_width: (native_width as f64 / scale).round().max(1.0) as u32,
        logical_height: (native_height as f64 / scale).round().max(1.0) as u32,
    }))
}

fn bounded_dimensions(width: u32, height: u32) -> (u32, u32) {
    let factor = 1.0_f64
        .min(f64::from(MAX_WIDTH) / f64::from(width.max(1)))
        .min(f64::from(MAX_HEIGHT) / f64::from(height.max(1)));
    let scaled_width = (f64::from(width) * factor).floor() as u32;
    let scaled_height = (f64::from(height) * factor).floor() as u32;
    (scaled_width.max(2) & !1, scaled_height.max(2) & !1)
}

/// Controls a user-authorized Wayland desktop session.
pub struct Host {
    display: String,
    runtime_dir: String,
    output: OutputInfo,
    input: WaylandInput,
}

impl Host {
    /// Connects to the current user's Wayland session and checks its helpers.
    pub fn new() -> Result<Self, HostError> {
        let display = std::env::var("WAYLAND_DISPLAY")
            .map_err(|_| io::Error::new(io::ErrorKind::NotConnected, "WAYLAND_DISPLAY is unset"))?;
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| io::Error::new(io::ErrorKind::NotConnected, "XDG_RUNTIME_DIR is unset"))?;
        let socket = if display.starts_with('/') {
            std::path::PathBuf::from(&display)
        } else {
            std::path::Path::new(&runtime_dir).join(&display)
        };
        if !socket.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Wayland socket does not exist",
            )
            .into());
        }
        for program in ["grim", "ffmpeg", "wl-copy", "wl-paste"] {
            if !command_exists(program) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("required program not found: {program}"),
                )
                .into());
            }
        }
        let output = read_output_info(&display, &runtime_dir)?;
        let input = WaylandInput::connect(
            output.name.as_deref(),
            output.logical_width,
            output.logical_height,
        )?;
        Ok(Self {
            display,
            runtime_dir,
            output,
            input,
        })
    }

    /// Starts continuous capture of the selected output, capped at UHD and encoded as
    /// low-latency H.264 Annex B. Dropping `reader` stops the capture pipeline.
    pub fn start_video(&mut self, profile: StreamProfile) -> Result<VideoSource, HostError> {
        let encoder = select_encoder()?;
        let (width, height) = profile.limit_dimensions(self.output.width, self.output.height);
        let fps = profile.fps().unwrap_or(FPS).clamp(1, 60);
        let gop = if profile == StreamProfile::Desktop {
            fps.saturating_mul(2)
        } else {
            fps
        };
        let rate_control = profile.bitrate_kbps().map_or_else(String::new, |bitrate| {
            format!(
                "-b:v {bitrate}k -maxrate {bitrate}k -bufsize {}k",
                bitrate.saturating_mul(2)
            )
        });
        let frame_delay = format!("{:.6}", 1.0 / f64::from(fps));
        let filter = encoder.filter_for(width, height);
        let grim = if self.output.name.is_some() {
            "grim -o \"$REMOTE_DESKTOP_OUTPUT\" -s \"$REMOTE_DESKTOP_SCALE\" -t ppm -"
        } else {
            "grim -t ppm -"
        };
        // `grim` emits PPM frames on stdout. This loop has a fixed rate and
        // streams directly into ffmpeg, so memory use is bounded to pipe buffers.
        let script = format!(
            "while :; do {grim} || exit $?; sleep {frame_delay}; done | ffmpeg -hide_banner -loglevel error -nostdin {device} -threads 1 -filter_threads 1 -use_wallclock_as_timestamps 1 -f image2pipe -framerate {fps} -vcodec ppm -i pipe:0 -vf '{filter}' -an -c:v {codec} {preset} {tune} {rate_control} -r {fps} -fps_mode cfr -g {gop} -bf 0 -pix_fmt {pix_fmt} -flush_packets 1 -bsf:v h264_metadata=aud=insert,dump_extra=freq=keyframe -f h264 pipe:1",
            frame_delay = frame_delay,
            grim = grim,
            device = encoder.device,
            filter = filter,
            codec = encoder.codec,
            preset = encoder.preset,
            tune = encoder.tune,
            rate_control = rate_control,
            fps = fps,
            gop = gop,
            pix_fmt = encoder.pix_fmt,
        );
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(script)
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(output) = &self.output.name {
            cmd.env("REMOTE_DESKTOP_OUTPUT", output)
                .env("REMOTE_DESKTOP_SCALE", format!("{}", self.output.scale));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Keep the shell, ffmpeg, and grim in a private process group so
            // they can all be stopped when the consumer drops the stream.
            cmd.process_group(0);
        }
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("ffmpeg stdout unavailable"))?;
        let stop = Box::new(move || terminate_process_group(pid));
        Ok(VideoSource {
            width,
            height,
            reader: Box::new(CaptureReader { child, reader }),
            stop,
        })
    }

    /// Sends virtual pointer and keyboard input to the active Wayland session.
    pub fn input(&mut self, event: InputEvent) -> Result<(), HostError> {
        self.input.send(event)
    }

    /// Reads plain text from the active session clipboard, if present.
    pub fn clipboard_text(&mut self) -> Result<Option<String>, HostError> {
        let mut child = Command::new("wl-paste")
            .args(["--no-newline", "--type", "text/plain"])
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut text = Vec::with_capacity(MAX_CLIPBOARD_TEXT.min(4096));
        child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("wl-paste stdout unavailable"))?
            .take((MAX_CLIPBOARD_TEXT + 1) as u64)
            .read_to_end(&mut text)?;
        if text.len() > MAX_CLIPBOARD_TEXT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clipboard text exceeds protocol limit",
            )
            .into());
        }
        if !child.wait()?.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8(text)?))
    }

    /// Sets plain text on the active session clipboard.
    pub fn set_clipboard_text(&mut self, text: &str) -> Result<(), HostError> {
        if text.len() > MAX_CLIPBOARD_TEXT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "clipboard text exceeds protocol limit",
            )
            .into());
        }
        let mut child = Command::new("wl-copy")
            .args(["--type", "text/plain;charset=utf-8"])
            .env("WAYLAND_DISPLAY", &self.display)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("wl-copy stdin unavailable"))?;
        let write_result = stdin.write_all(text.as_bytes());
        drop(stdin);
        let status = child.wait()?;
        write_result?;
        if !status.success() {
            return Err(io::Error::other(format!("wl-copy exited with status {status}")).into());
        }
        Ok(())
    }
}

struct InputState {
    pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    outputs: HashMap<u32, wl_output::WlOutput>,
    output_names: HashMap<u32, String>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for InputState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_manager = Some(registry.bind(name, version.min(2), qh, ()))
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboard_manager = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "wl_output" => {
                    let output = registry.bind(name, version.min(4), qh, name);
                    state.outputs.insert(name, output);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for InputState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names.insert(*global_name, name);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerManagerV1,
        _: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &ZwlrVirtualPointerV1,
        _: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for InputState {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct WaylandInput {
    connection: Connection,
    queue: EventQueue<InputState>,
    state: InputState,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    xkb_state: xkbcommon::xkb::State,
    started: Instant,
    pointer_width: u32,
    pointer_height: u32,
}

impl WaylandInput {
    fn connect(
        output_name: Option<&str>,
        pointer_width: u32,
        pointer_height: u32,
    ) -> Result<Self, HostError> {
        let connection = Connection::connect_to_env()?;
        let mut queue = connection.new_event_queue::<InputState>();
        let qh = queue.handle();
        let _registry = connection.display().get_registry(&qh, ());
        let mut state = InputState {
            pointer_manager: None,
            keyboard_manager: None,
            seat: None,
            outputs: HashMap::new(),
            output_names: HashMap::new(),
        };
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        let pointer_manager = state.pointer_manager.as_ref().cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "compositor does not expose zwlr_virtual_pointer_manager_v1",
            )
        })?;
        let keyboard_manager = state.keyboard_manager.as_ref().cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "compositor does not expose zwp_virtual_keyboard_manager_v1",
            )
        })?;
        let seat = state.seat.as_ref().cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "compositor does not expose wl_seat",
            )
        })?;
        let output = output_name.and_then(|name| {
            state.output_names.iter().find_map(|(global, current)| {
                (current == name)
                    .then(|| state.outputs.get(global))
                    .flatten()
            })
        });
        let pointer = pointer_manager.create_virtual_pointer_with_output(None, output, &qh, ());
        let keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        let (keymap, xkb_state) = build_keymap()?;
        let keymap_file = keymap_memfd(&keymap)?;
        let keymap_size = u32::try_from(keymap.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "keymap too large"))?;
        keyboard.keymap(1, keymap_file.as_fd(), keymap_size);
        connection.flush()?;
        Ok(Self {
            connection,
            queue,
            state,
            pointer,
            keyboard,
            xkb_state,
            started: Instant::now(),
            pointer_width,
            pointer_height,
        })
    }

    fn send(&mut self, event: InputEvent) -> Result<(), HostError> {
        let time = self.started.elapsed().as_millis() as u32;
        match event {
            InputEvent::PointerMove { x, y } => {
                let x = (x.clamp(0.0, 1.0) * self.pointer_width as f32).round() as u32;
                let y = (y.clamp(0.0, 1.0) * self.pointer_height as f32).round() as u32;
                use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::Request;
                self.pointer.send_request(Request::MotionAbsolute {
                    time,
                    x,
                    y,
                    x_extent: self.pointer_width,
                    y_extent: self.pointer_height,
                })?;
                self.pointer.frame();
            }
            InputEvent::PointerButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left => 0x110,
                    MouseButton::Right => 0x111,
                    MouseButton::Middle => 0x112,
                    MouseButton::Back => 0x116,
                    MouseButton::Forward => 0x115,
                };
                let state = if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                self.pointer.button(time, code, state);
                self.pointer.frame();
            }
            InputEvent::Scroll { x, y } => {
                if x != 0.0 {
                    self.pointer.axis(
                        time,
                        wl_pointer::Axis::HorizontalScroll,
                        f64::from(x) * 120.0,
                    );
                }
                if y != 0.0 {
                    self.pointer.axis(
                        time,
                        wl_pointer::Axis::VerticalScroll,
                        f64::from(-y) * 120.0,
                    );
                }
                self.pointer.frame();
            }
            InputEvent::Key { code, pressed } => {
                let key = evdev_code(&code).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unsupported physical key code: {code}"),
                    )
                })?;
                use xkbcommon::xkb::{KeyDirection, Keycode};
                let xkb_key = Keycode::new(key + 8);
                self.xkb_state.update_key(
                    xkb_key,
                    if pressed {
                        KeyDirection::Down
                    } else {
                        KeyDirection::Up
                    },
                );
                use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::Request as KeyboardRequest;
                self.keyboard.send_request(KeyboardRequest::Modifiers {
                    mods_depressed: self
                        .xkb_state
                        .serialize_mods(xkbcommon::xkb::STATE_MODS_DEPRESSED),
                    mods_latched: self
                        .xkb_state
                        .serialize_mods(xkbcommon::xkb::STATE_MODS_LATCHED),
                    mods_locked: self
                        .xkb_state
                        .serialize_mods(xkbcommon::xkb::STATE_MODS_LOCKED),
                    group: self
                        .xkb_state
                        .serialize_layout(xkbcommon::xkb::STATE_LAYOUT_EFFECTIVE),
                })?;
                self.keyboard.key(time, key, u32::from(pressed));
            }
        }
        self.connection.flush()?;
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }
}

fn build_keymap() -> Result<(String, xkbcommon::xkb::State), HostError> {
    use xkbcommon::xkb;
    let layout = std::env::var("REMOTE_DESKTOP_XKB_LAYOUT")
        .or_else(|_| std::env::var("XKB_DEFAULT_LAYOUT"))
        .unwrap_or_else(|_| "us".to_owned());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "evdev",
        "pc105",
        &layout,
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot compile XKB keymap for layout {layout}"),
        )
    })?;
    let state = xkb::State::new(&keymap);
    Ok((keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1), state))
}

fn keymap_memfd(contents: &str) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "remote-linux-host-keymap-{}-{nonce}",
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    file.write_all(contents.as_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

fn evdev_code(code: &str) -> Option<u32> {
    Some(match code {
        "KeyA" => 30,
        "KeyB" => 48,
        "KeyC" => 46,
        "KeyD" => 32,
        "KeyE" => 18,
        "KeyF" => 33,
        "KeyG" => 34,
        "KeyH" => 35,
        "KeyI" => 23,
        "KeyJ" => 36,
        "KeyK" => 37,
        "KeyL" => 38,
        "KeyM" => 50,
        "KeyN" => 49,
        "KeyO" => 24,
        "KeyP" => 25,
        "KeyQ" => 16,
        "KeyR" => 19,
        "KeyS" => 31,
        "KeyT" => 20,
        "KeyU" => 22,
        "KeyV" => 47,
        "KeyW" => 17,
        "KeyX" => 45,
        "KeyY" => 21,
        "KeyZ" => 44,
        "Digit1" => 2,
        "Digit2" => 3,
        "Digit3" => 4,
        "Digit4" => 5,
        "Digit5" => 6,
        "Digit6" => 7,
        "Digit7" => 8,
        "Digit8" => 9,
        "Digit9" => 10,
        "Digit0" => 11,
        "Enter" => 28,
        "Escape" => 1,
        "Backspace" => 14,
        "Tab" => 15,
        "Space" => 57,
        "Minus" => 12,
        "Equal" => 13,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Backslash" => 43,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "CapsLock" => 58,
        "ShiftLeft" => 42,
        "ShiftRight" => 54,
        "ControlLeft" => 29,
        "ControlRight" => 97,
        "AltLeft" => 56,
        "AltRight" => 100,
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ArrowUp" => 103,
        "ArrowDown" => 108,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "Delete" => 111,
        "Insert" => 110,
        "Home" => 102,
        "End" => 107,
        "PageUp" => 104,
        "PageDown" => 109,
        "PrintScreen" => 99,
        "ScrollLock" => 70,
        "Pause" => 119,
        "NumLock" => 69,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        "F13" => 183,
        "F14" => 184,
        "F15" => 185,
        "F16" => 186,
        "Numpad0" => 82,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadAdd" => 78,
        "NumpadSubtract" => 74,
        "NumpadMultiply" => 55,
        "NumpadDivide" => 98,
        "NumpadDecimal" => 83,
        "NumpadEnter" => 96,
        _ => return None,
    })
}

struct Encoder {
    codec: &'static str,
    preset: &'static str,
    tune: &'static str,
    device: &'static str,
    pix_fmt: &'static str,
}

impl Encoder {
    fn filter_for(&self, width: u32, height: u32) -> String {
        match self.codec {
            "h264_vaapi" => format!("format=nv12,hwupload,scale_vaapi=w={width}:h={height}"),
            "h264_nvenc" => format!("scale={width}:{height}:flags=fast_bilinear,format=nv12"),
            _ => format!("scale={width}:{height}:flags=fast_bilinear"),
        }
    }
}

fn select_encoder() -> Result<Encoder, HostError> {
    let requested = std::env::var("REMOTE_DESKTOP_H264_ENCODER").ok();
    let encoders = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()?;
    let listing = String::from_utf8_lossy(&encoders.stdout);
    let candidates: Vec<&str> = if let Some(name) = requested.as_deref() {
        vec![name]
    } else if std::path::Path::new("/dev/dri/renderD128").exists() {
        vec!["h264_vaapi", "h264_nvenc", "libx264"]
    } else {
        vec!["h264_nvenc", "libx264"]
    };
    for name in candidates {
        if !listing.contains(name) {
            continue;
        }
        let encoder = match name {
            "h264_vaapi" => Encoder {
                codec: "h264_vaapi",
                preset: "-quality 7",
                tune: "",
                device: "-vaapi_device /dev/dri/renderD128",
                pix_fmt: "vaapi",
            },
            "h264_nvenc" => Encoder {
                codec: "h264_nvenc",
                preset: "-preset p1",
                tune: "-tune ll",
                device: "",
                pix_fmt: "nv12",
            },
            "libx264" => Encoder {
                codec: "libx264",
                preset: "-preset ultrafast",
                tune: "-tune zerolatency",
                device: "",
                pix_fmt: "yuv420p",
            },
            _ => continue,
        };
        if probe_encoder(&encoder) {
            return Ok(encoder);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no working H.264 ffmpeg encoder found",
    )
    .into())
}

fn probe_encoder(encoder: &Encoder) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(encoder.device.split_whitespace())
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=1",
            "-frames:v",
            "1",
            "-vf",
            &encoder.filter_for(64, 64),
        ]);
    cmd.args(["-c:v", encoder.codec])
        .args(encoder.preset.split_whitespace())
        .args(encoder.tune.split_whitespace())
        .args(["-pix_fmt", encoder.pix_fmt, "-f", "h264", "-y", "/dev/null"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().is_ok_and(|status| status.success())
}

fn command_exists(program: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join(program).is_file())
}

struct CaptureReader {
    child: Child,
    reader: std::process::ChildStdout,
}

impl Read for CaptureReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Drop for CaptureReader {
    fn drop(&mut self) {
        terminate_process_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn terminate_process_group(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::parse_output_info;

    #[test]
    fn chooses_enabled_current_1280x720_output() {
        let json = br#"[
          {"name":"DP-1","enabled":false,"scale":1.0,"transform":"normal","modes":[{"width":3840,"height":2160,"current":true}]},
          {"name":"HDMI-A-1","enabled":true,"scale":1.0,"transform":"normal","modes":[{"width":1280,"height":720,"current":true},{"width":1920,"height":1080,"preferred":true}]}
        ]"#;
        let info = parse_output_info(json, None).unwrap().unwrap();
        assert_eq!(info.name.as_deref(), Some("HDMI-A-1"));
        assert_eq!((info.width, info.height), (1280, 720));
    }

    #[test]
    fn caps_uhd_while_preserving_wide_aspect_ratio() {
        let json = br#"[
          {"name":"DP-1","enabled":true,"scale":2.0,"transform":"normal","modes":[{"width":7680,"height":2160,"current":true}]}
        ]"#;
        let info = parse_output_info(json, Some("DP-1")).unwrap().unwrap();
        assert_eq!((info.width, info.height), (3840, 1080));
        assert_eq!((info.logical_width, info.logical_height), (3840, 1080));
    }
}
