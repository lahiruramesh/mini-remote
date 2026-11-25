use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32};

use crate::workspace;
use remote_protocol::{diagnostics::Monitor, ResourceUsage};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_SAMPLES: usize = 60;

#[derive(Default)]
pub struct Diagnostics {
    pub open: bool,
    local_monitor: Option<Monitor>,
    local_unavailable: bool,
    remote_unavailable: bool,
    local: VecDeque<ResourceUsage>,
    remote: VecDeque<ResourceUsage>,
    last_remote: Option<Instant>,
    last_poll: Option<(Instant, u64, u64)>,
    throughput: f64,
    fps: f64,
}

impl Diagnostics {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.last_poll = None;
        if self.open {
            self.local_unavailable = false;
        } else {
            self.local_monitor = None;
        }
    }

    pub fn tick(&mut self, bytes: u64, frames: u64) -> bool {
        if !self.open {
            return false;
        }
        if self.local_monitor.is_none() && !self.local_unavailable {
            self.local_monitor = Monitor::start().ok();
            self.local_unavailable = self.local_monitor.is_none();
        }
        if let Some(sample) = self.local_monitor.as_ref().and_then(Monitor::poll) {
            self.local_unavailable = sample.is_none();
            if let Some(sample) = sample {
                push(&mut self.local, sample);
            }
        }
        let now = Instant::now();
        if let Some((previous, old_bytes, old_frames)) = self.last_poll {
            let seconds = now.duration_since(previous).as_secs_f64();
            if seconds < SAMPLE_INTERVAL.as_secs_f64() {
                return false;
            }
            self.throughput = bytes.saturating_sub(old_bytes) as f64 * 8.0 / seconds / 1e6;
            self.fps = frames.saturating_sub(old_frames) as f64 / seconds;
        }
        self.last_poll = Some((now, bytes, frames));
        if let Some(monitor) = &self.local_monitor {
            monitor.request();
        }
        true
    }

    pub fn remote_sample(&mut self, sample: Option<ResourceUsage>) {
        self.last_remote = Some(Instant::now());
        self.remote_unavailable = sample.is_none();
        if let Some(sample) = sample {
            push(&mut self.remote, sample);
        }
    }

    pub fn reset_connection(&mut self) {
        self.remote.clear();
        self.last_remote = None;
        self.remote_unavailable = false;
        self.last_poll = None;
        self.throughput = 0.0;
        self.fps = 0.0;
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        resolution: Option<(u32, u32)>,
        remote_supported: bool,
    ) {
        ui.horizontal(|ui| {
            ui.label(workspace::title("Diagnostics", 17.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if workspace::icon(ui, workspace::Icon::Close, "Close diagnostics", false).clicked()
                {
                    self.toggle();
                }
            });
        });
        ui.add_space(3.0);
        if let Some((width, height)) = resolution {
            workspace::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(workspace::title("Stream", 13.0));
                ui.label(workspace::muted(format!("{width} × {height}  /  H.264")));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    reading(ui, "Receive", &format!("{:.1} Mbps", self.throughput));
                    ui.separator();
                    reading(ui, "Display", &format!("{:.1} fps", self.fps));
                });
            });
        }
        section(
            ui,
            "Viewer",
            "This machine",
            &self.local,
            self.local_unavailable
                .then_some("Resource sampling is unavailable."),
        );
        let remote_message = if resolution.is_none() {
            Some("Connect to a host to sample its connector.")
        } else if !remote_supported {
            Some("Update this host’s connector to get its resource usage.")
        } else if self.remote_unavailable {
            Some("The connector could not collect a sample.")
        } else if self
            .last_remote
            .is_some_and(|last| last.elapsed() > Duration::from_secs(4))
        {
            Some("Waiting for a fresh connector sample.")
        } else {
            None
        };
        section(
            ui,
            "Connector",
            "Remote machine",
            &self.remote,
            remote_message,
        );
        ui.add_space(3.0);
        ui.label(workspace::muted(
            "CPU uses one logical core as 100%. RAM is process RSS. GPU memory is separate.",
        ));
        if ui.button("Clear history").clicked() {
            self.local.clear();
            self.remote.clear();
        }
    }
}

fn push(history: &mut VecDeque<ResourceUsage>, sample: ResourceUsage) {
    if history.len() == MAX_SAMPLES {
        history.pop_front();
    }
    history.push_back(sample);
}

fn memory(sample: &ResourceUsage) -> f64 {
    sample
        .main
        .rss_bytes
        .saturating_add(sample.media.rss_bytes)
        .saturating_add(sample.helpers.rss_bytes) as f64
        / 1_048_576.0
}

fn cpu(sample: &ResourceUsage) -> Option<f32> {
    Some(sample.main.cpu_percent? + sample.media.cpu_percent? + sample.helpers.cpu_percent?)
}

fn reading(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(workspace::muted(label));
        ui.label(workspace::title(value, 16.0));
    });
}

fn section(
    ui: &mut egui::Ui,
    heading: &str,
    location: &str,
    history: &VecDeque<ResourceUsage>,
    message: Option<&str>,
) {
    workspace::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(workspace::title(heading, 14.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(workspace::muted(location));
            });
        });
        if let Some(message) = message {
            ui.label(workspace::muted(message));
            return;
        }
        let Some(sample) = history.back() else {
            ui.label(workspace::muted("Collecting a sample…"));
            return;
        };
        ui.horizontal(|ui| {
            reading(ui, "Memory", &format!("{:.0} MiB", memory(sample)));
            ui.separator();
            reading(ui, "CPU", &percent(cpu(sample)));
        });
        sparkline(
            ui,
            "RAM",
            history.iter().map(|sample| Some(memory(sample) as f32)),
            workspace::ACCENT,
        );
        sparkline(
            ui,
            "CPU",
            history.iter().map(cpu),
            Color32::from_rgb(131, 158, 246),
        );
        egui::CollapsingHeader::new("Process breakdown")
            .id_salt(heading)
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new(heading).spacing([9.0, 5.0]).show(ui, |ui| {
                    for (name, group) in [
                        ("App", sample.main),
                        ("FFmpeg", sample.media),
                        ("Other helpers", sample.helpers),
                    ] {
                        ui.label(workspace::muted(name));
                        ui.label(format!("{:.1} MiB", group.rss_bytes as f64 / 1_048_576.0));
                        ui.label(percent(group.cpu_percent));
                        ui.end_row();
                    }
                });
            });
    });
}

fn percent(usage: Option<f32>) -> String {
    usage.map_or_else(|| "—".into(), |value| format!("{value:.1}%"))
}

fn sparkline(
    ui: &mut egui::Ui,
    title: &str,
    values: impl Iterator<Item = Option<f32>>,
    color: Color32,
) {
    ui.horizontal(|ui| {
        ui.label(workspace::muted(title));
        let values: Vec<_> = values.collect();
        let maximum = values.iter().copied().flatten().fold(1.0_f32, f32::max);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, workspace::CANVAS);
        let points: Vec<_> = values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                let level = value.filter(|value| value.is_finite())?;
                Some(egui::pos2(
                    rect.left() + rect.width() * index as f32 / (MAX_SAMPLES - 1) as f32,
                    rect.bottom() - 2.0 - (rect.height() - 4.0) * level / maximum,
                ))
            })
            .collect();
        if points.len() > 1 {
            painter.add(egui::Shape::line(points, egui::Stroke::new(1.5_f32, color)));
        } else if let Some(point) = points.first() {
            painter.circle_filled(*point, 2.0, color);
        }
    });
}
