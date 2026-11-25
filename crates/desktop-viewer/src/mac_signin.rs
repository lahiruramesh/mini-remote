//! Optional Linux bridge for macOS Screen Sharing's native sign-in dialog.
//!
//! The viewer only proxies bytes between a local TigerVNC client and the
//! verified Tailscale peer. Authentication is handled entirely by TigerVNC.

use std::{
    io,
    net::{Shutdown, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const SCREEN_SHARING_PORT: u16 = 5900;

pub fn launch(host: String, status: mpsc::Sender<super::NetEvent>) -> Result<(), String> {
    let viewer = find_viewer()
        .ok_or_else(|| "Install TigerVNC (`xtigervncviewer`) to use Mac sign-in.".to_owned())?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("Could not open local sign-in bridge: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let mut child = Command::new(viewer)
        .args([
            "-SecurityTypes=DH",
            "-SendClipboard=0",
            "-AcceptClipboard=0",
            "-SendPrimary=0",
            "-ReconnectOnError=0",
            &format!("127.0.0.1::{port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not launch TigerVNC: {e}"))?;

    thread::spawn(move || {
        let _ = status.send(super::NetEvent::MacSignIn {
            message: "Waiting for TigerVNC sign-in window…".into(),
            active: true,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let local = loop {
            if let Ok(Some(exit)) = child.try_wait() {
                let _ = status.send(super::NetEvent::MacSignIn {
                    message: format!("TigerVNC exited ({exit}) before connecting."),
                    active: false,
                });
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(75))
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let _ = status.send(super::NetEvent::MacSignIn {
                        message: "TigerVNC did not open the Mac sign-in connection.".into(),
                        active: false,
                    });
                    stop(&mut child);
                    return;
                }
                Err(error) => {
                    let _ = status.send(super::NetEvent::MacSignIn {
                        message: format!("Local sign-in bridge failed: {error}"),
                        active: false,
                    });
                    stop(&mut child);
                    return;
                }
            }
        };

        let _ = status.send(super::NetEvent::MacSignIn {
            message: "Connecting to Mac Screen Sharing over Tailscale…".into(),
            active: true,
        });
        match remote_protocol::tailnet::connect(&host, SCREEN_SHARING_PORT) {
            Ok(remote) => {
                let _ = status.send(super::NetEvent::MacSignIn {
                    message: "Mac sign-in window connected; credentials stay in TigerVNC.".into(),
                    active: true,
                });
                relay(local, remote);
                let _ = status.send(super::NetEvent::MacSignIn {
                    message: "Mac sign-in session ended.".into(),
                    active: false,
                });
            }
            Err(error) => {
                let _ = status.send(super::NetEvent::MacSignIn {
                    message: format!("Could not reach Mac Screen Sharing over Tailscale: {error}"),
                    active: false,
                });
                stop(&mut child);
            }
        }
        stop(&mut child);
    });
    Ok(())
}

fn find_viewer() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH").unwrap_or_default();
    let installed = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin"));
    installed
        .into_iter()
        .chain(std::env::split_paths(&path))
        .find_map(|directory| {
            ["xtigervncviewer", "vncviewer"].iter().find_map(|binary| {
                let candidate = directory.join(binary);
                let metadata = std::fs::metadata(&candidate).ok()?;
                (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                    .then_some(candidate)
            })
        })
}

fn relay(mut local: TcpStream, mut remote: TcpStream) {
    let Ok(mut local_reader) = local.try_clone() else {
        return;
    };
    let Ok(mut remote_writer) = remote.try_clone() else {
        return;
    };
    thread::scope(|scope| {
        scope.spawn(move || {
            let _ = io::copy(&mut local_reader, &mut remote_writer);
            let _ = local_reader.shutdown(Shutdown::Both);
            let _ = remote_writer.shutdown(Shutdown::Both);
        });
        let _ = io::copy(&mut remote, &mut local);
        let _ = local.shutdown(Shutdown::Both);
        let _ = remote.shutdown(Shutdown::Both);
    });
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
