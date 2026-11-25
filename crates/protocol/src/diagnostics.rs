use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::thread;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use super::{ProcessUsage, ResourceUsage};

/// Sampling runs outside the UI, input and video loops. Only one sample can wait
/// in either direction, so requests cannot queue behind a slow process refresh.
pub struct Monitor {
    requests: mpsc::SyncSender<()>,
    samples: mpsc::Receiver<Option<ResourceUsage>>,
}

impl Monitor {
    pub fn start() -> std::io::Result<Self> {
        let (requests, work) = mpsc::sync_channel(1);
        let (output, samples) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("resource-sampler".into())
            .spawn(move || {
                let mut sampler = System::new().ok();
                let mut has_previous_sample = false;
                while work.recv().is_ok() {
                    let sample = sampler.as_mut().and_then(|system| {
                        let sample = collect(system, has_previous_sample);
                        has_previous_sample = sample.is_some();
                        sample
                    });
                    if let Err(mpsc::TrySendError::Disconnected(_)) = output.try_send(sample) {
                        break;
                    }
                }
            })?;
        Ok(Self { requests, samples })
    }

    pub fn request(&self) {
        let _ = self.requests.try_send(());
    }

    pub fn poll(&self) -> Option<Option<ResourceUsage>> {
        self.samples.try_recv().ok()
    }
}

fn collect(system: &mut System, has_previous_sample: bool) -> Option<ResourceUsage> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .without_tasks(),
    );
    let root = Pid::from_u32(std::process::id());
    let root_process = system.process(root)?;
    let mut children = HashMap::<Pid, Vec<Pid>>::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut usage = ResourceUsage {
        main: ProcessUsage {
            rss_bytes: root_process.memory(),
            cpu_percent: cpu(root_process, has_previous_sample),
        },
        ..ResourceUsage::default()
    };
    let mut seen = HashSet::from([root]);
    let mut pending = children.remove(&root).unwrap_or_default();
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(process) = system.process(pid) {
            usage.child_processes += 1;
            let group = if process.name() == "ffmpeg" {
                &mut usage.media
            } else {
                &mut usage.helpers
            };
            group.rss_bytes = group.rss_bytes.saturating_add(process.memory());
            if let Some(cpu) = cpu(process, has_previous_sample) {
                group.cpu_percent = Some(group.cpu_percent.unwrap_or_default() + cpu);
            }
        }
        pending.extend(children.remove(&pid).unwrap_or_default());
    }
    if has_previous_sample {
        usage.media.cpu_percent.get_or_insert(0.0);
        usage.helpers.cpu_percent.get_or_insert(0.0);
    }
    Some(usage)
}

fn cpu(process: &sysinfo::Process, warmed_up: bool) -> Option<f32> {
    let usage = process.cpu_usage();
    (warmed_up && usage.is_finite() && usage >= 0.0).then_some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_the_current_process_without_a_first_sample_cpu_value() {
        let mut system = System::new().unwrap();
        let sample = collect(&mut system, false).unwrap();
        assert!(sample.main.rss_bytes > 0);
        assert!(sample.main.cpu_percent.is_none());
    }
}
