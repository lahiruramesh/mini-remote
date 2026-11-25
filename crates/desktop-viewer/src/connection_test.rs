use std::time::{Duration, Instant};

use remote_protocol::{
    read_server, tailnet, write_client, ClientMessage, ServerMessage, StreamProfile,
};

pub fn run(host: &str, port: u16, token: &str, profile: StreamProfile) -> Result<String, String> {
    let mut socket = tailnet::connect(host, port)
        .map_err(|error| format!("Couldn’t reach the connector on port {port}. {error}"))?;
    if token.trim().is_empty() {
        return Ok(format!("Port {port} is reachable over Tailscale. Enter a pairing token to verify desktop video."));
    }
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    write_client(
        &mut socket,
        &ClientMessage::Authenticate {
            token: token.trim().to_owned(),
            clipboard: false,
            profile,
            phone_workspace: false,
            viewport: None,
        },
    )
    .map_err(|error| error.to_string())?;

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut resolution = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("The connector responded but desktop video did not arrive in time.".into());
        }
        socket
            .set_read_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        match read_server(&mut socket) {
            Ok(ServerMessage::Ready { width, height, .. }) => resolution = Some((width, height)),
            Ok(ServerMessage::Error { message }) => return Err(message),
            Ok(ServerMessage::Video(_)) if resolution.is_some() => {
                let (width, height) = resolution.unwrap();
                return Ok(format!(
                    "Tailscale, pairing, and desktop video passed at {width} × {height}."
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(format!(
                    "The port is reachable, but the desktop test ended. {error}"
                ))
            }
        }
    }
}
