use std::io::{self, Read, Write};
use std::net::IpAddr;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod diagnostics;
pub mod tailnet;

pub const DEFAULT_PORT: u16 = 47990;
pub const MAX_VIDEO_CHUNK: usize = 64 * 1024;
pub const MAX_CLIPBOARD_TEXT: usize = 128 * 1024;
const MAX_CONTROL: usize = MAX_CLIPBOARD_TEXT * 6 + 256;
const VERSION: u8 = 1;
const CONTROL: u8 = 0;
const VIDEO: u8 = 1;

pub struct VideoSource {
    pub viewport: Option<VideoViewport>,
    pub width: u32,
    pub height: u32,
    pub reader: Box<dyn Read + Send>,
    pub stop: Box<dyn FnOnce() + Send>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct ViewportRequest {
    pub width: u16,
    pub height: u16,
    pub position: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewportAxis {
    None,
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
pub struct VideoViewport {
    pub axis: ViewportAxis,
    pub width: u32,
    pub height: u32,
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
}

impl ViewportRequest {
    pub fn fit(self, width: u32, height: u32) -> VideoViewport {
        let aspect = f64::from(self.width) / f64::from(self.height);
        let native = f64::from(width) / f64::from(height);
        let mut viewport = VideoViewport {
            axis: ViewportAxis::None,
            width,
            height,
            scale_x: 1.0,
            scale_y: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        let position = f32::from(self.position) / 100.0;
        if native > aspect {
            viewport.width = ((f64::from(height) * aspect) as u32 & !1).clamp(2, width);
            viewport.scale_x = viewport.width as f32 / width as f32;
            viewport.offset_x = (1.0 - viewport.scale_x) * position;
            if viewport.width != width {
                viewport.axis = ViewportAxis::Horizontal;
            }
        } else if native < aspect {
            viewport.height = ((f64::from(width) / aspect) as u32 & !1).clamp(2, height);
            viewport.scale_y = viewport.height as f32 / height as f32;
            viewport.offset_y = (1.0 - viewport.scale_y) * position;
            if viewport.height != height {
                viewport.axis = ViewportAxis::Vertical;
            }
        }
        viewport
    }
}

impl VideoViewport {
    pub fn crop_filter(self) -> String {
        if self.axis == ViewportAxis::None {
            return String::new();
        }
        format!(
            "crop=round(iw*{}/2)*2:round(ih*{}/2)*2:iw*{}:ih*{}:exact=1,",
            f64::from(self.scale_x),
            f64::from(self.scale_y),
            self.offset_x,
            self.offset_y,
        )
    }

    pub fn map_pointer(self, x: f32, y: f32) -> InputEvent {
        InputEvent::PointerMove {
            x: (self.offset_x + x * self.scale_x).clamp(0.0, 1.0),
            y: (self.offset_y + y * self.scale_y).clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum InputEvent {
    PointerMove { x: f32, y: f32 },
    PointerButton { button: MouseButton, pressed: bool },
    Scroll { x: f32, y: f32 },
    Key { code: String, pressed: bool },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowShortcut {
    NextApp,
    PreviousApp,
    CloseWindow,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ProcessUsage {
    pub rss_bytes: u64,
    pub cpu_percent: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ResourceUsage {
    pub main: ProcessUsage,
    pub media: ProcessUsage,
    pub helpers: ProcessUsage,
    pub child_processes: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamProfile {
    #[default]
    Desktop,
    LowData,
    Balanced,
    Sharp,
}

impl StreamProfile {
    pub fn limit_dimensions(self, width: u32, height: u32) -> (u32, u32) {
        let (max_width, max_height) = match self {
            Self::Desktop => return (width, height),
            Self::LowData => (854.0, 480.0),
            Self::Balanced => (1280.0, 720.0),
            Self::Sharp => (1920.0, 1080.0),
        };
        let (max_width, max_height) = if height > width {
            (max_height, max_width)
        } else {
            (max_width, max_height)
        };
        let scale = (max_width / f64::from(width))
            .min(max_height / f64::from(height))
            .min(1.0);
        (
            ((f64::from(width) * scale).floor() as u32).max(2) & !1,
            ((f64::from(height) * scale).floor() as u32).max(2) & !1,
        )
    }

    pub fn fps(self) -> Option<u32> {
        match self {
            Self::Desktop => None,
            Self::LowData => Some(15),
            Self::Balanced => Some(24),
            Self::Sharp => Some(30),
        }
    }

    pub fn bitrate_kbps(self) -> Option<u32> {
        match self {
            Self::Desktop => None,
            Self::LowData => Some(850),
            Self::Balanced => Some(2200),
            Self::Sharp => Some(4800),
        }
    }
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(tag = "message", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    Authenticate {
        token: String,
        clipboard: bool,
        #[serde(default)]
        profile: StreamProfile,
        #[serde(default)]
        phone_workspace: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewport: Option<ViewportRequest>,
    },
    RedeemInvite {
        code: String,
    },
    Input(InputEvent),
    WindowShortcut {
        shortcut: WindowShortcut,
    },
    Clipboard {
        text: String,
    },
    PasteText {
        text: String,
    },
    ClipboardEnabled {
        enabled: bool,
    },
    Ping,
    RequestDiagnostics,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "message", content = "data", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        width: u32,
        height: u32,
        #[serde(default)]
        diagnostics: bool,
        #[serde(default)]
        profile: StreamProfile,
        #[serde(default)]
        phone_workspace: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewport_axis: Option<ViewportAxis>,
    },
    Paired {
        token: String,
    },
    Diagnostics {
        usage: Option<ResourceUsage>,
    },
    #[serde(skip)]
    Video(Vec<u8>),
    Clipboard {
        text: String,
    },
    ClipboardStatus {
        enabled: bool,
    },
    InputStatus {
        enabled: bool,
    },
    Error {
        message: String,
    },
    Pong,
}

pub fn is_tailnet_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            first == 100 && (64..=127).contains(&second)
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

pub fn read_client(reader: &mut impl Read) -> io::Result<ClientMessage> {
    let (_, payload) = read_packet(reader, false)?;
    let message = parse::<ClientMessage>(&payload)?;
    validate_client(&message)?;
    Ok(message)
}

pub fn write_client(writer: &mut impl Write, message: &ClientMessage) -> io::Result<()> {
    validate_client(message)?;
    write_control(writer, message)
}

pub fn read_server(reader: &mut impl Read) -> io::Result<ServerMessage> {
    let (kind, payload) = read_packet(reader, true)?;
    if kind == VIDEO {
        return Ok(ServerMessage::Video(payload));
    }
    let message = parse::<ServerMessage>(&payload)?;
    validate_server(&message)?;
    Ok(message)
}

pub fn write_server(writer: &mut impl Write, message: &ServerMessage) -> io::Result<()> {
    validate_server(message)?;
    match message {
        ServerMessage::Video(bytes) => write_video(writer, bytes),
        _ => write_control(writer, message),
    }
}

pub fn write_video(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_VIDEO_CHUNK {
        return Err(invalid("invalid video chunk size"));
    }
    write_packet(writer, VIDEO, bytes)
}

fn validate_client(message: &ClientMessage) -> io::Result<()> {
    match message {
        ClientMessage::Authenticate { token, .. } if token.len() > 128 => {
            Err(invalid("token too large"))
        }
        ClientMessage::Authenticate {
            viewport: Some(viewport),
            ..
        } if !(2..=4096).contains(&viewport.width)
            || !(2..=4096).contains(&viewport.height)
            || viewport.position > 100 =>
        {
            Err(invalid("invalid video viewport"))
        }
        ClientMessage::RedeemInvite { code }
            if code.len() != 32 || !code.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Err(invalid("invalid invitation code"))
        }
        ClientMessage::Clipboard { text } | ClientMessage::PasteText { text }
            if text.len() > MAX_CLIPBOARD_TEXT =>
        {
            Err(invalid("clipboard text too large"))
        }
        ClientMessage::Input(InputEvent::PointerMove { x, y })
            if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) =>
        {
            Err(invalid("invalid pointer position"))
        }
        ClientMessage::Input(InputEvent::Scroll { x, y })
            if !(-100.0..=100.0).contains(x) || !(-100.0..=100.0).contains(y) =>
        {
            Err(invalid("invalid scroll distance"))
        }
        ClientMessage::Input(InputEvent::Key { code, .. })
            if code.is_empty()
                || code.len() > 40
                || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) =>
        {
            Err(invalid("invalid key code"))
        }
        _ => Ok(()),
    }
}

fn validate_server(message: &ServerMessage) -> io::Result<()> {
    match message {
        ServerMessage::Ready { width, height, .. }
            if !(1..=7680).contains(width) || !(1..=4320).contains(height) =>
        {
            Err(invalid("invalid display size"))
        }
        ServerMessage::Clipboard { text } if text.len() > MAX_CLIPBOARD_TEXT => {
            Err(invalid("clipboard text too large"))
        }
        _ => Ok(()),
    }
}

fn read_packet(reader: &mut impl Read, allow_video: bool) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0_u8; 6];
    reader.read_exact(&mut header)?;
    if header[0] != VERSION {
        return Err(invalid("unsupported protocol version"));
    }
    let limit = match header[1] {
        CONTROL => MAX_CONTROL,
        VIDEO if allow_video => MAX_VIDEO_CHUNK,
        _ => return Err(invalid("unexpected packet kind")),
    };
    let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    if size == 0 || size > limit {
        return Err(invalid("invalid packet size"));
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload)?;
    Ok((header[1], payload))
}

fn write_control<T: Serialize>(writer: &mut impl Write, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_CONTROL {
        return Err(invalid("control message too large"));
    }
    write_packet(writer, CONTROL, &bytes)
}

fn write_packet(writer: &mut impl Write, kind: u8, bytes: &[u8]) -> io::Result<()> {
    let size = (bytes.len() as u32).to_be_bytes();
    writer.write_all(&[VERSION, kind, size[0], size[1], size[2], size[3]])?;
    writer.write_all(bytes)
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_profiles_keep_ultrawide_and_portrait_aspect_ratios() {
        assert_eq!(
            StreamProfile::LowData.limit_dimensions(3440, 1440),
            (854, 356)
        );
        assert_eq!(
            StreamProfile::Balanced.limit_dimensions(1080, 1920),
            (720, 1280)
        );
        assert_eq!(
            StreamProfile::Desktop.limit_dimensions(3440, 1440),
            (3440, 1440)
        );
    }

    #[test]
    fn legacy_authentication_defaults_to_desktop_profile() {
        let auth = br#"{"message":"authenticate","data":{"token":"secret","clipboard":false}}"#;
        let mut packets = Vec::new();
        write_packet(&mut packets, CONTROL, auth).unwrap();
        assert!(matches!(
            read_client(&mut packets.as_slice()).unwrap(),
            ClientMessage::Authenticate {
                profile: StreamProfile::Desktop,
                phone_workspace: false,
                viewport: None,
                ..
            }
        ));
    }

    #[test]
    fn fits_portrait_view_and_maps_pointer_to_the_visible_region() {
        let view = ViewportRequest {
            width: 500,
            height: 1000,
            position: 100,
        }
        .fit(3440, 1440);
        assert_eq!(view.axis, ViewportAxis::Horizontal);
        assert_eq!(
            StreamProfile::Balanced.limit_dimensions(view.width, view.height),
            (640, 1280)
        );
        let InputEvent::PointerMove { x, y } = view.map_pointer(0.0, 0.5) else {
            unreachable!()
        };
        assert!((x - 2720.0 / 3440.0).abs() < 0.0001);
        assert_eq!(y, 0.5);
        assert_eq!(
            view.map_pointer(1.0, 1.0),
            InputEvent::PointerMove { x: 1.0, y: 1.0 }
        );
    }

    #[test]
    fn fits_landscape_view_and_maps_pointer_when_the_desktop_is_taller() {
        let view = ViewportRequest {
            width: 1000,
            height: 500,
            position: 0,
        }
        .fit(1920, 1080);
        assert_eq!(view.axis, ViewportAxis::Vertical);
        assert_eq!((view.width, view.height), (1920, 960));
        assert_eq!(
            view.map_pointer(0.5, 0.0),
            InputEvent::PointerMove { x: 0.5, y: 0.0 }
        );
        let matching = ViewportRequest {
            width: 16,
            height: 9,
            position: 50,
        }
        .fit(1920, 1080);
        assert_eq!(matching.axis, ViewportAxis::None);
        assert!(matching.crop_filter().is_empty());
    }

    #[test]
    fn a_legacy_connector_does_not_advertise_diagnostics() {
        let legacy = br#"{"message":"ready","data":{"width":1920,"height":1080}}"#;
        let mut packets = Vec::new();
        write_packet(&mut packets, CONTROL, legacy).unwrap();
        assert_eq!(
            read_server(&mut packets.as_slice()).unwrap(),
            ServerMessage::Ready {
                width: 1920,
                height: 1080,
                diagnostics: false,
                profile: StreamProfile::Desktop,
                phone_workspace: false,
                viewport_axis: None,
            },
        );
    }

    #[test]
    fn interleaved_video_and_unicode_clipboard_round_trip() {
        let mut packets = Vec::new();
        let clipboard = ServerMessage::Clipboard {
            text: "Hej 👋 ආයුබෝවන්".into(),
        };
        write_video(&mut packets, &[0, 0, 0, 1, 103]).unwrap();
        write_server(&mut packets, &clipboard).unwrap();
        let mut packets = packets.as_slice();
        assert_eq!(
            read_server(&mut packets).unwrap(),
            ServerMessage::Video(vec![0, 0, 0, 1, 103])
        );
        assert_eq!(read_server(&mut packets).unwrap(), clipboard);
        assert!(packets.is_empty());
    }

    #[test]
    fn rejects_oversized_packet_before_reading_payload() {
        let mut header: &[u8] = &[VERSION, CONTROL, 255, 255, 255, 255];
        assert_eq!(
            read_client(&mut header).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn checks_tailnet_address_ranges() {
        for allowed in ["100.64.0.0", "100.127.255.255", "fd7a:115c:a1e0::42"] {
            assert!(is_tailnet_ip(allowed.parse().unwrap()));
        }
        for denied in [
            "100.63.255.255",
            "100.128.0.0",
            "192.168.0.1",
            "fd7a:115c:a1e1::42",
            "::1",
        ] {
            assert!(!is_tailnet_ip(denied.parse().unwrap()));
        }
    }
}
