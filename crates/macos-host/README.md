# macOS host backend

`remote-macos-host` supplies the `remote_protocol::Host` backend for macOS.
It captures the active desktop with FFmpeg's AVFoundation input and encodes
H.264 with Apple's VideoToolbox through FFmpeg. The returned reader emits
Annex B bytes; dropping the reader or invoking `VideoSource.stop` kills and
reaps the FFmpeg capture process.

## Requirements and permissions

- macOS with a logged-in, unlocked user session.
- FFmpeg on `PATH` (or `REMOTE_MACOS_FFMPEG`) built with AVFoundation input
  and `h264_videotoolbox` support. The host refuses VideoToolbox software
  fallback (`-allow_sw 0`).
- macOS Screen Recording permission for the application that launches this
  backend. `screen_permission_granted()` and `Host::start_video` check access
  without prompting. An application can call `request_screen_permission()`
  after an explicit user action to open the system flow.
  The OS owns the permission prompt and setting; this crate does not change
  privacy preferences.
- Accessibility permission may be needed by macOS to accept synthesized
  keyboard and pointer events. Input is performed through Enigo. `Host::new`
  remains usable without input permission so video-only sessions still work;
  `input_permission_status()` reports CoreGraphics event-posting access and
  Enigo's Accessibility trust separately without prompting;
  `input_permission_granted()` requires both. Input is lazily
  initialized with Enigo prompts disabled. An application can call
  `request_input_permission()` after an explicit user action to invoke the
  matching system permission flow.

FFmpeg resolves from `REMOTE_MACOS_FFMPEG`, `PATH`, `/opt/homebrew/bin/ffmpeg`,
then `/usr/local/bin/ffmpeg`. The default AVFoundation input is
`Capture screen 0`; override it with
`REMOTE_MACOS_VIDEO_DEVICE` if the installed FFmpeg build lists it differently.
FFmpeg lists available devices with:

```sh
ffmpeg -f avfoundation -list_devices true -i ""
```

Defaults are 30 fps, a bitrate scaled from resolution and frame rate (8 Mbps
at 1080p30, capped at 32 Mbps), and the physical display resolution capped at
3840x2160 while preserving aspect ratio. AVFoundation requests NV12; FFmpeg
uploads frames to VideoToolbox, scales with `scale_vt`, and encodes on the
hardware path. Software encoding fallback is disabled. Configure `REMOTE_MACOS_WIDTH` and
`REMOTE_MACOS_HEIGHT` together to request another fixed output size,
`REMOTE_MACOS_FPS` for 1–60 fps, and `REMOTE_MACOS_BITRATE` for FFmpeg's bitrate
syntax (for example `5M`). The display receives normalized pointer coordinates
independently of the encoded size.

This backend captures the interactive desktop session. It does not implement
pre-login or lock-screen capture; native OS sharing remains responsible for
that session lifecycle.

During local investigation, macOS Screen Sharing answered on TCP 5900 and
offered RFB security types 30 (Apple account authentication) and 2 (legacy VNC
password authentication), among Apple extensions. A Linux viewer needs Apple's
type 30 implementation to authenticate with a macOS account. TigerVNC 1.14.1+
and current noVNC list Apple Diffie-Hellman support; type 2 requires a
separately configured VNC compatibility password. This backend does not enable
or change Screen Sharing settings. Use that built-in service as the alternate
login/lock-screen viewer path; the Rust host stream is for an active user
desktop session.
