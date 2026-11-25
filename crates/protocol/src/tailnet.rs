use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use socket2::{Domain, Protocol, Socket, Type};

use crate::is_tailnet_ip;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    this_device: Device,
    peer: Option<std::collections::HashMap<String, Device>>,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<IpAddr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Device {
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<IpAddr>,
}

pub struct Tailnet {
    local: Vec<IpAddr>,
    peers: Vec<Device>,
}

impl Tailnet {
    pub fn discover() -> io::Result<Self> {
        let output = tailscale_status()?;
        let status = serde_json::from_slice::<Status>(&output).map_err(io::Error::other)?;
        if status.backend_state != "Running" {
            return Err(io::Error::other("Tailscale is not connected"));
        }
        let mut local = status.this_device.tailscale_ips;
        if local.is_empty() {
            local = status.tailscale_ips;
        }
        local.retain(|address| is_tailnet_ip(*address));
        if local.is_empty() {
            return Err(io::Error::other("Tailscale has no local address"));
        }
        Ok(Self {
            local,
            peers: status.peer.unwrap_or_default().into_values().collect(),
        })
    }

    pub fn local_ipv4(&self) -> io::Result<IpAddr> {
        self.local
            .iter()
            .copied()
            .find(IpAddr::is_ipv4)
            .ok_or_else(|| io::Error::other("Tailscale has no local IPv4 address"))
    }

    pub fn contains_peer(&self, address: IpAddr) -> bool {
        is_tailnet_ip(address)
            && self
                .peers
                .iter()
                .any(|peer| peer.tailscale_ips.contains(&address))
    }

    fn resolve(&self, address: &str) -> io::Result<Vec<IpAddr>> {
        let address = address.trim().trim_end_matches('.');
        if let Ok(ip) = address.parse() {
            return if self.contains_peer(ip) || self.local.contains(&ip) {
                Ok(vec![ip])
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "address is not a device on this tailnet",
                ))
            };
        }

        let mut devices = self.peers.iter().filter(|device| {
            let dns_name = device.dns_name.trim_end_matches('.');
            let short = dns_name.split('.').next().unwrap_or("");
            address.eq_ignore_ascii_case(dns_name)
                || address.eq_ignore_ascii_case(short)
                || address.eq_ignore_ascii_case(&device.host_name)
        });
        let device = devices.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "device is absent from Tailscale status",
            )
        })?;
        if devices.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ambiguous device name; enter its full MagicDNS name",
            ));
        }

        let mut addresses: Vec<_> = device
            .tailscale_ips
            .iter()
            .copied()
            .filter(|ip| is_tailnet_ip(*ip))
            .collect();
        addresses.sort_by_key(IpAddr::is_ipv6);
        Ok(addresses)
    }
}

pub fn connect(address: &str, port: u16) -> io::Result<TcpStream> {
    let tailnet = Tailnet::discover()?;
    let mut failure = io::Error::new(
        io::ErrorKind::NotFound,
        "device has no matching Tailscale address",
    );
    for destination in tailnet.resolve(address)? {
        if let Some(source) = tailnet
            .local
            .iter()
            .copied()
            .find(|source| source.is_ipv4() == destination.is_ipv4())
        {
            match connect_from(source, SocketAddr::new(destination, port)) {
                Ok(stream) => return Ok(stream),
                Err(error) => failure = error,
            }
        }
    }
    Err(failure)
}

fn connect_from(source: IpAddr, destination: SocketAddr) -> io::Result<TcpStream> {
    let socket = Socket::new(
        Domain::for_address(destination),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    socket.bind(&SocketAddr::new(source, 0).into())?;
    socket.connect_timeout(&destination.into(), Duration::from_secs(3))?;
    let stream = TcpStream::from(socket);
    stream.set_nodelay(true)?;
    Ok(stream)
}

fn tailscale_status() -> io::Result<Vec<u8>> {
    let output = if let Some(executable) = std::env::var_os("MINIREMOTE_TAILSCALE") {
        Command::new(executable).args(["status", "--json"]).output()
    } else {
        Command::new("tailscale")
            .args(["status", "--json"])
            .output()
            .or_else(|error| {
                if cfg!(target_os = "macos") && error.kind() == io::ErrorKind::NotFound {
                    Command::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale")
                        .args(["status", "--json"])
                        .output()
                } else {
                    Err(error)
                }
            })
    }?;
    if !output.status.success() {
        return Err(io::Error::other("could not read Tailscale status"));
    }
    Ok(output.stdout)
}
