//! macOS host support for remote-desktop.
//!
//! Screen capture and encoding are delegated to FFmpeg's AVFoundation and
//! VideoToolbox backends. This keeps capture buffers on the native media path
//! and avoids copying raw frames through Rust. The FFmpeg executable must be
//! installed with AVFoundation input and `h264_videotoolbox` enabled.

use std::error::Error;
use std::ffi::c_void;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use remote_protocol::{InputEvent, MouseButton, StreamProfile, VideoSource};

type HostResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// The two macOS gates relevant to Enigo-based synthetic input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputPermissionStatus {
    /// Whether this process is trusted for Accessibility. Enigo uses this
    /// exact `AXIsProcessTrustedWithOptions` gate.
    pub accessibility_trusted: bool,
    /// Whether CoreGraphics allows this process to post events.
    pub post_event_access: bool,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

/// macOS desktop capture, input injection, and clipboard access.
pub struct Host {
    input: Option<Enigo>,
    desktop_width: u32,
    desktop_height: u32,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: String,
    scroll_x_fraction: f32,
    scroll_y_fraction: f32,
}

impl Host {
    /// Connects to macOS input APIs and reads capture settings from the
    /// `REMOTE_MACOS_*` environment variables.
    ///
    /// Screen Recording permission is checked without prompting when capture
    /// starts. The app can call [`request_screen_capture_permission`] after an
    /// explicit user action to invoke macOS's permission flow.
    pub fn new() -> HostResult<Self> {
        let display = unsafe { CGMainDisplayID() };
        let desktop_width = u32::try_from(unsafe { CGDisplayPixelsWide(display) })
            .unwrap_or(1280)
            .max(1);
        let desktop_height = u32::try_from(unsafe { CGDisplayPixelsHigh(display) })
            .unwrap_or(720)
            .max(1);
        let (width, height) = output_dimensions(desktop_width, desktop_height);
        let fps = std::env::var("REMOTE_MACOS_FPS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| (1..=60).contains(value))
            .unwrap_or(30);
        let bitrate = std::env::var("REMOTE_MACOS_BITRATE")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default_bitrate(width, height, fps));

        Ok(Self {
            // Defer Enigo initialization until the first input event. A host
            // can stream video even when the user has not granted input access.
            input: None,
            desktop_width,
            desktop_height,
            width,
            height,
            fps,
            bitrate,
            scroll_x_fraction: 0.0,
            scroll_y_fraction: 0.0,
        })
    }

    /// Starts a low-latency H.264 Annex B stream from the main display.
    ///
    /// Defaults to the AVFoundation device named `Capture screen 0`; set
    /// `REMOTE_MACOS_VIDEO_DEVICE` to override it for FFmpeg builds with a
    /// different device listing. Output dimensions default to the main
    /// display’s physical pixel size capped to 3840×2160. Set width and
    /// height explicitly to choose another fixed stream size.
    pub fn start_video(&mut self, profile: StreamProfile) -> HostResult<VideoSource> {
        // This is a non-prompting check. Permission requests must stay with
        // the app's explicit screen-sharing flow, never happen as a side effect
        // of accepting a remote connection.
        if !screen_permission_granted() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "macOS Screen Recording permission is not available to this host process",
            )
            .into());
        }
        let (width, height) = profile.limit_dimensions(self.width, self.height);
        let fps = profile.fps().unwrap_or(self.fps).clamp(1, 60);
        let bitrate = profile
            .bitrate_kbps()
            .map(|kbps| format!("{kbps}k"))
            .unwrap_or_else(|| self.bitrate.clone());
        let rate_control = profile.bitrate_kbps().map_or_else(String::new, |kbps| {
            format!("-maxrate {kbps}k -bufsize {}k", kbps.saturating_mul(2))
        });
        let gop = fps;
        let device = std::env::var("REMOTE_MACOS_VIDEO_DEVICE")
            .unwrap_or_else(|_| "Capture screen 0".to_owned());
        let ffmpeg = ffmpeg_executable();
        let filter = format!("format=nv12,hwupload,scale_vt=w={}:h={}", width, height);
        let input = format!("{device}:none");
        let mut child = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "warning",
                "-init_hw_device",
                "videotoolbox=vt",
                "-filter_hw_device",
                "vt",
                "-f",
                "avfoundation",
                "-framerate",
            ])
            .arg(fps.to_string())
            .args(["-pixel_format", "nv12", "-capture_cursor", "1", "-i"])
            .arg(input)
            .args(["-an", "-vf", &filter, "-fps_mode", "cfr"])
            .args([
                "-c:v",
                "h264_videotoolbox",
                "-realtime",
                "1",
                "-allow_sw",
                "0",
                "-b:v",
            ])
            .arg(&bitrate)
            .args(rate_control.split_whitespace())
            .args([
                "-g",
                &gop.to_string(),
                "-bf",
                "0",
                "-profile:v",
                "baseline",
                "-r",
                &fps.to_string(),
                "-bsf:v",
                "h264_metadata=aud=insert,dump_extra=freq=keyframe",
                "-f",
                "h264",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "FFmpeg stdout was not piped",
                )
                .into());
            }
        };
        let child = Arc::new(Mutex::new(child));
        let stop_child = Arc::clone(&child);

        Ok(VideoSource {
            width,
            height,
            reader: Box::new(FfmpegReader { child, stdout }),
            stop: Box::new(move || stop_process(&stop_child)),
        })
    }

    /// Injects a pointer, keyboard, button, or scroll event.
    pub fn input(&mut self, event: InputEvent) -> HostResult<()> {
        if self.input.is_none() {
            if !unsafe { CGPreflightPostEventAccess() } {
                return Err(permission_error(
                    "macOS does not currently allow this process to post input events",
                ));
            }
            self.input = Some(new_enigo(false).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("macOS input access is unavailable: {error}"),
                )
            })?);
        }
        let input = self.input.as_mut().expect("Enigo was just initialized");
        match event {
            InputEvent::PointerMove { x, y } => {
                let x = normalized_coordinate(x, self.desktop_width);
                let y = normalized_coordinate(y, self.desktop_height);
                input.move_mouse(x, y, Coordinate::Abs)?;
            }
            InputEvent::PointerButton { button, pressed } => {
                let button = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Right,
                    MouseButton::Back => Button::Back,
                    MouseButton::Forward => Button::Forward,
                };
                input.button(button, direction(pressed))?;
            }
            InputEvent::Scroll { x, y } => {
                if x.is_finite() {
                    let (ticks, remainder) = accumulate_scroll(self.scroll_x_fraction, x);
                    self.scroll_x_fraction = remainder;
                    if ticks != 0 {
                        input.scroll(ticks, Axis::Horizontal)?;
                    }
                }
                if y.is_finite() {
                    // Enigo's positive vertical scroll is down; protocol is up-positive.
                    let (ticks, remainder) = accumulate_scroll(self.scroll_y_fraction, -y);
                    self.scroll_y_fraction = remainder;
                    if ticks != 0 {
                        input.scroll(ticks, Axis::Vertical)?;
                    }
                }
            }
            InputEvent::Key { code, pressed } => {
                let (key, keycode) = map_key_code(&code).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unsupported physical key code: {code}"),
                    )
                })?;
                if let Some(key) = key {
                    input.key(key, direction(pressed))?;
                } else if let Some(keycode) = keycode {
                    input.raw(keycode, direction(pressed))?;
                }
            }
        }
        Ok(())
    }

    /// Reads text from the macOS pasteboard, if it currently contains text.
    pub fn clipboard_text(&mut self) -> HostResult<Option<String>> {
        let mut clipboard = Clipboard::new()?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(error) => Err(Box::new(error)),
        }
    }

    /// Replaces the macOS pasteboard's text contents.
    pub fn set_clipboard_text(&mut self, text: &str) -> HostResult<()> {
        Clipboard::new()?.set_text(text.to_owned())?;
        Ok(())
    }
}

/// Returns whether macOS currently permits this process to synthesize input.
/// This check does not show a prompt.
pub fn input_permission_granted() -> bool {
    let status = input_permission_status();
    status.accessibility_trusted && status.post_event_access
}

/// Reports the native gates independently so the packaged app can explain
/// exactly which permission is missing. This never prompts.
pub fn input_permission_status() -> InputPermissionStatus {
    InputPermissionStatus {
        // NULL means no prompt. This is the exact AX trust gate Enigo wraps,
        // without conflating permission state with Enigo initialization.
        accessibility_trusted: unsafe { AXIsProcessTrustedWithOptions(std::ptr::null()) },
        post_event_access: unsafe { CGPreflightPostEventAccess() },
    }
}

/// Returns whether macOS currently permits this process to capture the
/// screen. This check does not show a prompt.
pub fn screen_permission_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Asks macOS to show its standard Screen Recording permission flow.
/// Call this only after an explicit user action from the packaged host app.
pub fn request_screen_capture_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Alias for [`request_screen_capture_permission`] for connector CLI use.
pub fn request_screen_permission() -> bool {
    request_screen_capture_permission()
}

fn ffmpeg_executable() -> PathBuf {
    if let Some(path) = std::env::var_os("REMOTE_MACOS_FFMPEG") {
        return PathBuf::from(path);
    }

    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join("ffmpeg"))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }

    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

/// Asks macOS to show its standard system-owned input permission flow.
/// Call this only after an explicit user action from the packaged host app.
pub fn request_input_permission() -> bool {
    // Enigo's exact gate is AXIsProcessTrustedWithOptions. The CoreGraphics
    // PostEvent gate is separate on current macOS, so request both explicitly.
    let accessibility_trusted = new_enigo(true).is_ok();
    let post_event_access =
        unsafe { CGPreflightPostEventAccess() } || unsafe { CGRequestPostEventAccess() };
    accessibility_trusted && post_event_access && input_permission_granted()
}

fn new_enigo(prompt: bool) -> Result<Enigo, enigo::NewConError> {
    Enigo::new(&Settings {
        open_prompt_to_get_permissions: prompt,
        ..Settings::default()
    })
}

fn permission_error(message: &'static str) -> Box<dyn Error + Send + Sync> {
    io::Error::new(io::ErrorKind::PermissionDenied, message).into()
}

fn output_dimensions(source_width: u32, source_height: u32) -> (u32, u32) {
    let configured_width = std::env::var("REMOTE_MACOS_WIDTH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (16..=7680).contains(value));
    let configured_height = std::env::var("REMOTE_MACOS_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (16..=7680).contains(value));
    match (configured_width, configured_height) {
        (Some(width), Some(height)) => (even(width), even(height)),
        _ => {
            let scale = (3840.0 / source_width as f64)
                .min(2160.0 / source_height as f64)
                .min(1.0);
            (
                even((source_width as f64 * scale).round() as u32).max(16),
                even((source_height as f64 * scale).round() as u32).max(16),
            )
        }
    }
}

fn default_bitrate(width: u32, height: u32, fps: u32) -> String {
    let pixels_per_second = u64::from(width) * u64::from(height) * u64::from(fps);
    let reference = 1920_u64 * 1080 * 30;
    let bitrate_mbps = ((pixels_per_second.saturating_mul(8) / reference).clamp(5, 32)) as u32;
    format!("{bitrate_mbps}M")
}

fn even(value: u32) -> u32 {
    value & !1
}

fn normalized_coordinate(value: f32, extent: u32) -> i32 {
    let normalized = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (normalized * extent.saturating_sub(1) as f32).round() as i32
}

fn accumulate_scroll(previous: f32, delta: f32) -> (i32, f32) {
    let total = (previous + delta).clamp(i32::MIN as f32, i32::MAX as f32);
    let ticks = total.trunc() as i32;
    (ticks, total - ticks as f32)
}

fn direction(pressed: bool) -> Direction {
    if pressed {
        Direction::Press
    } else {
        Direction::Release
    }
}

/// Maps W3C `KeyboardEvent.code` values to macOS virtual keycodes. Raw
/// keycodes preserve physical-key semantics across keyboard layouts.
fn map_key_code(code: &str) -> Option<(Option<Key>, Option<u16>)> {
    let key = match code {
        "Escape" => Some(Key::Escape),
        "Backspace" => Some(Key::Backspace),
        "Tab" => Some(Key::Tab),
        "Enter" => Some(Key::Return),
        "Space" => Some(Key::Space),
        "Delete" => Some(Key::Delete),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "ArrowLeft" => Some(Key::LeftArrow),
        "ArrowRight" => Some(Key::RightArrow),
        "ArrowUp" => Some(Key::UpArrow),
        "ArrowDown" => Some(Key::DownArrow),
        "CapsLock" => Some(Key::CapsLock),
        _ => None,
    };
    if let Some(key) = key {
        return Some((Some(key), None));
    }

    let raw = match code {
        // Keep left/right modifier keys distinct so releasing one side while
        // the other remains held does not synthesize an early key-up.
        "ShiftLeft" => 0x38,
        "ShiftRight" => 0x3C,
        "ControlLeft" => 0x3B,
        "ControlRight" => 0x3E,
        "AltLeft" => 0x3A,
        "AltRight" => 0x3D,
        "MetaLeft" => 0x37,
        "MetaRight" => 0x36,
        // ANSI US physical positions; these are Apple's virtual keycodes.
        "KeyA" => 0x00,
        "KeyS" => 0x01,
        "KeyD" => 0x02,
        "KeyF" => 0x03,
        "KeyH" => 0x04,
        "KeyG" => 0x05,
        "KeyZ" => 0x06,
        "KeyX" => 0x07,
        "KeyC" => 0x08,
        "KeyV" => 0x09,
        "KeyB" => 0x0B,
        "KeyQ" => 0x0C,
        "KeyW" => 0x0D,
        "KeyE" => 0x0E,
        "KeyR" => 0x0F,
        "KeyY" => 0x10,
        "KeyT" => 0x11,
        "Digit1" => 0x12,
        "Digit2" => 0x13,
        "Digit3" => 0x14,
        "Digit4" => 0x15,
        "Digit6" => 0x16,
        "Digit5" => 0x17,
        "Equal" => 0x18,
        "Digit9" => 0x19,
        "Digit7" => 0x1A,
        "Minus" => 0x1B,
        "Digit8" => 0x1C,
        "Digit0" => 0x1D,
        "BracketRight" => 0x1E,
        "KeyO" => 0x1F,
        "KeyU" => 0x20,
        "BracketLeft" => 0x21,
        "KeyI" => 0x22,
        "KeyP" => 0x23,
        "KeyL" => 0x25,
        "KeyJ" => 0x26,
        "Quote" => 0x27,
        "KeyK" => 0x28,
        "Semicolon" => 0x29,
        "Backslash" => 0x2A,
        "Comma" => 0x2B,
        "Slash" => 0x2C,
        "KeyN" => 0x2D,
        "KeyM" => 0x2E,
        "Period" => 0x2F,
        "Backquote" => 0x32,
        "F1" => 0x7A,
        "F2" => 0x78,
        "F3" => 0x63,
        "F4" => 0x76,
        "F5" => 0x60,
        "F6" => 0x61,
        "F7" => 0x62,
        "F8" => 0x64,
        "F9" => 0x65,
        "F10" => 0x6D,
        "F11" => 0x67,
        "F12" => 0x6F,
        "F13" => 0x69,
        "F14" => 0x6B,
        "F15" => 0x71,
        "F16" => 0x6A,
        "F17" => 0x40,
        "F18" => 0x4F,
        "F19" => 0x50,
        "F20" => 0x5A,
        "Numpad0" => 0x52,
        "Numpad1" => 0x53,
        "Numpad2" => 0x54,
        "Numpad3" => 0x55,
        "Numpad4" => 0x56,
        "Numpad5" => 0x57,
        "Numpad6" => 0x58,
        "Numpad7" => 0x59,
        "Numpad8" => 0x5B,
        "Numpad9" => 0x5C,
        "NumpadDecimal" => 0x41,
        "NumpadMultiply" => 0x43,
        "NumpadAdd" => 0x45,
        "NumpadDivide" => 0x4B,
        "NumpadEnter" => 0x4C,
        "NumpadSubtract" => 0x4E,
        "NumpadEqual" => 0x51,
        _ => return None,
    };
    Some((None, Some(raw)))
}

struct FfmpegReader {
    child: Arc<Mutex<Child>>,
    stdout: ChildStdout,
}

impl Read for FfmpegReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

impl Drop for FfmpegReader {
    fn drop(&mut self) {
        // Closing the pipe should stop FFmpeg itself. Kill as a fallback to
        // make source teardown deterministic if AVFoundation is still live.
        stop_process(&self.child);
    }
}

fn stop_process(child: &Arc<Mutex<Child>>) {
    if let Ok(mut child) = child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
