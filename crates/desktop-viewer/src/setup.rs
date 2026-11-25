use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use qrcode::{Color, QrCode};

use crate::workspace;

#[derive(Clone, Copy, PartialEq)]
enum Platform {
    Mac,
    Linux,
}

pub struct Guide {
    platform: Platform,
    pairing: Option<PairingPanel>,
}

impl Default for Guide {
    fn default() -> Self {
        Self {
            pairing: None,
            platform: if cfg!(target_os = "macos") {
                Platform::Mac
            } else {
                Platform::Linux
            },
        }
    }
}

impl Guide {
    pub fn pair_phone() -> Self {
        Self {
            pairing: Some(PairingPanel::start()),
            ..Self::default()
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        if let Some(panel) = &mut self.pairing {
            if panel.show(ui) {
                self.pairing = None;
            }
            return false;
        }
        let mut close = false;
        let width = (ui.available_width() - 32.0).clamp(290.0, 600.0);
        ui.vertical_centered(|ui| {
            ui.add_space(34.0);
            ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(width);
                ui.label(workspace::title("Set up Mini Remote", 26.0));
                ui.label(workspace::muted("Connect to another computer or pair this desktop with your phone."));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.platform, Platform::Mac, "macOS");
                    ui.selectable_value(&mut self.platform, Platform::Linux, "Linux");
                });
                ui.add_space(8.0);
                if ui.add(workspace::primary("Pair an iPhone or iPad")).clicked() {
                    self.pairing = Some(PairingPanel::start());
                }
                ui.add_space(8.0);
                workspace::card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    step(ui, "1", "Join the same Tailscale network", |ui| {
                        ui.label("Install Tailscale on the new device, sign in, and look up its Tailscale address.");
                    });
                    ui.separator();
                    step(ui, "2", "Install and start the connector", |ui| {
                        ui.label("With Rust, Python 3, and FFmpeg installed, run this from the Mini Remote source directory.");
                        command(ui, "python3 scripts/package.py --install --autostart");
                        match self.platform {
                            Platform::Mac => ui.label("In System Settings → Privacy & Security, allow Mini Remote Connector under Screen & System Audio Recording and Accessibility."),
                            Platform::Linux => ui.label("Use a compatible Wayland desktop such as Budgie with labwc. Install grim, wlr-randr, and wl-clipboard, and keep a graphical session running."),
                        };
                    });
                    ui.separator();
                    step(ui, "3", "Pair and test from this viewer", |ui| {
                        ui.label("Run this on the host to show its pairing token.");
                        command(ui, match self.platform {
                            Platform::Mac => "\"$HOME/Applications/MiniRemote Connector.app/Contents/MacOS/remote-connector\" pair",
                            Platform::Linux => "~/.local/bin/remote-connector pair",
                        });
                        ui.label("Enter the host and token in the connection form. Select Test connection, then Connect.");
                    });
                });
                ui.add_space(7.0);
                if ui.add(workspace::primary("Done")).clicked() {
                    close = true;
                }
            });
        });
        close
    }
}

fn step(ui: &mut egui::Ui, number: &str, label: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).color(workspace::ACCENT).strong());
        ui.label(workspace::title(label, 14.0));
    });
    ui.add_space(3.0);
    content(ui);
    ui.add_space(5.0);
}

fn command(ui: &mut egui::Ui, command: &str) {
    egui::Frame::none()
        .fill(workspace::SIDEBAR)
        .rounding(7.0)
        .inner_margin(egui::Margin::symmetric(9.0, 6.0))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(command).monospace()).selectable(true))
        });
}

enum PairingPanel {
    Generating(mpsc::Receiver<Result<QrCode, String>>),
    Ready {
        code: egui::TextureHandle,
        expires: Instant,
    },
    Error(String),
}

impl PairingPanel {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(pairing_code());
        });
        Self::Generating(receiver)
    }

    fn show(&mut self, ui: &mut egui::Ui) -> bool {
        if let Self::Generating(result) = self {
            match result.try_recv() {
                Ok(Ok(code)) => {
                    let width = code.width();
                    let side = width + 8;
                    let mut image = egui::ColorImage::new([side, side], egui::Color32::WHITE);
                    for y in 0..width {
                        for x in 0..width {
                            if code[(x, y)] == Color::Dark {
                                image[(x + 4, y + 4)] = egui::Color32::BLACK;
                            }
                        }
                    }
                    let code = ui.ctx().load_texture(
                        "mobile_pairing",
                        image,
                        egui::TextureOptions::NEAREST,
                    );
                    *self = Self::Ready {
                        code,
                        expires: Instant::now() + Duration::from_secs(178),
                    };
                }
                Ok(Err(error)) => *self = Self::Error(error),
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(Duration::from_millis(100))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    *self = Self::Error("Couldn’t start pairing.".into())
                }
            }
        }
        let mut back = false;
        let mut regenerate = false;
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(workspace::title("Pair this desktop", 24.0));
            ui.label("On your phone, open Mini Remote and tap Scan pairing code.");
            ui.add_space(12.0);
            match self {
                Self::Generating(_) => {
                    ui.spinner();
                }
                Self::Ready { code, expires } if *expires > Instant::now() => {
                    let size = ui.available_width().min(280.0);
                    ui.image((code.id(), egui::vec2(size, size)));
                    let remaining = expires.saturating_duration_since(Instant::now()).as_secs();
                    ui.label(format!(
                        "One use · expires in {}:{:02}",
                        remaining / 60,
                        remaining % 60
                    ));
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                }
                Self::Ready { .. } => {
                    ui.label("This code has expired.");
                }
                Self::Error(error) => {
                    ui.label(error.as_str());
                }
            }
            ui.add_space(8.0);
            if !matches!(self, Self::Generating(_)) && ui.button("Show a new code").clicked() {
                regenerate = true;
            }
            if ui.button("Back to setup").clicked() {
                back = true;
            }
        });
        if regenerate {
            *self = Self::start();
        }
        back
    }
}

fn pairing_code() -> Result<QrCode, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Couldn’t find your home folder.")?;
    let connector = if cfg!(target_os = "macos") {
        home.join("Applications/MiniRemote Connector.app/Contents/MacOS/remote-connector")
    } else {
        home.join(".local/bin/remote-connector")
    };
    let output = std::process::Command::new(connector)
        .arg("invite")
        .output()
        .map_err(|_| "Install the latest Mini Remote Connector on this desktop first.")?;
    if !output.status.success() {
        return Err(
            "Start the updated Mini Remote Connector and connect Tailscale on this desktop.".into(),
        );
    }
    QrCode::new(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout))
        .map_err(|_| "Couldn’t generate the pairing code.".into())
}
