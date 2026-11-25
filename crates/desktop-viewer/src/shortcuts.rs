use eframe::egui::{self, Key, Modifiers};
use remote_protocol::WindowShortcut;

pub const ACTIONS: [(&str, WindowShortcut); 3] = [
    ("Next app", WindowShortcut::NextApp),
    ("Previous app", WindowShortcut::PreviousApp),
    ("Close remote window", WindowShortcut::CloseWindow),
];

const STORAGE_KEYS: [&str; 3] = [
    "remote-desktop-viewer.shortcut.next-app",
    "remote-desktop-viewer.shortcut.previous-app",
    "remote-desktop-viewer.shortcut.close-window",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    key: Key,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
}

impl Binding {
    fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.mac_cmd,
        }
    }

    fn name(self) -> String {
        let mut parts = Vec::new();
        for (enabled, label) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.cmd, "Cmd"),
        ] {
            if enabled {
                parts.push(label);
            }
        }
        parts.push(self.key.name());
        parts.join("+")
    }

    fn encode(self) -> String {
        let mask = u8::from(self.ctrl)
            | (u8::from(self.alt) << 1)
            | (u8::from(self.shift) << 2)
            | (u8::from(self.cmd) << 3);
        format!("{mask} {}", self.key.name())
    }

    fn decode(value: &str) -> Option<Self> {
        let (mask, name) = value.split_once(' ')?;
        let mask = mask.parse::<u8>().ok().filter(|mask| *mask < 16)?;
        let key = Key::from_name(name).filter(|key| selectable(*key))?;
        Some(Self {
            key,
            ctrl: mask & 1 != 0,
            alt: mask & 2 != 0,
            shift: mask & 4 != 0,
            cmd: mask & 8 != 0,
        })
    }

    fn is_clipboard_shortcut(self) -> bool {
        let clipboard_modifier = if cfg!(target_os = "macos") {
            self.cmd
        } else {
            self.ctrl
        };
        clipboard_modifier && matches!(self.key, Key::C | Key::V | Key::X)
    }
}

fn selectable(key: Key) -> bool {
    super::key_code(key).is_some()
        && !matches!(key, Key::Colon | Key::Pipe | Key::Questionmark | Key::Plus)
}

#[derive(Clone, Copy)]
pub struct Bindings([Option<Binding>; ACTIONS.len()]);

impl Default for Bindings {
    fn default() -> Self {
        let modifiers = Modifiers::CTRL | Modifiers::SHIFT;
        Self([
            Some(Binding::new(Key::Tab, modifiers)),
            Some(Binding::new(Key::Backspace, modifiers)),
            None,
        ])
    }
}

impl Bindings {
    pub fn load(storage: &dyn eframe::Storage) -> Self {
        let mut bindings = Self::default();
        for (binding, key) in bindings.0.iter_mut().zip(STORAGE_KEYS) {
            match storage.get_string(key).as_deref() {
                None => {}
                Some("unassigned") => *binding = None,
                Some(value) => {
                    if let Some(saved) = Binding::decode(value) {
                        *binding = Some(saved);
                    }
                }
            }
        }
        if bindings.conflict().is_some() {
            Self::default()
        } else {
            bindings
        }
    }

    pub fn save(self, storage: &mut dyn eframe::Storage) {
        for (binding, key) in self.0.into_iter().zip(STORAGE_KEYS) {
            storage.set_string(
                key,
                binding.map_or_else(|| "unassigned".into(), Binding::encode),
            );
        }
    }

    pub fn action(self, key: Key, modifiers: Modifiers) -> Option<WindowShortcut> {
        let binding = Binding::new(key, modifiers);
        self.0
            .iter()
            .position(|stored| *stored == Some(binding))
            .map(|index| ACTIONS[index].1)
    }

    pub fn label(self, index: usize) -> String {
        let name = ACTIONS[index].0;
        self.0[index].map_or_else(
            || name.into(),
            |binding| format!("{name} · {}", binding.name()),
        )
    }

    fn conflict(self) -> Option<String> {
        for (index, binding) in self.0.iter().enumerate() {
            let Some(binding) = binding else { continue };
            if binding.is_clipboard_shortcut() {
                return Some(format!(
                    "{} is reserved for clipboard commands.",
                    binding.name()
                ));
            }
            if let Some(other) = self.0[..index]
                .iter()
                .position(|value| *value == Some(*binding))
            {
                return Some(format!(
                    "{} is assigned to both {} and {}.",
                    binding.name(),
                    ACTIONS[other].0,
                    ACTIONS[index].0,
                ));
            }
        }
        None
    }

    pub fn edit(&mut self, ui: &mut egui::Ui, recording: &mut Option<usize>) -> Option<bool> {
        use crate::workspace;

        if let Some(index) = *recording {
            let candidate = ui.input_mut(|input| {
                let found =
                    input
                        .events
                        .iter()
                        .enumerate()
                        .find_map(|(position, event)| match event {
                            egui::Event::Key {
                                physical_key: Some(key),
                                modifiers,
                                pressed: true,
                                repeat: false,
                                ..
                            } if *key == Key::Escape || selectable(*key) => {
                                Some((position, *key, *modifiers))
                            }
                            _ => None,
                        });
                found.map(|(position, key, modifiers)| {
                    input.events.remove(position);
                    (key, modifiers)
                })
            });
            if let Some((key, modifiers)) = candidate {
                if key == Key::Escape {
                    *recording = None;
                } else {
                    self.0[index] = Some(Binding::new(key, modifiers));
                    *recording = None;
                }
            }
        }

        let mut result = None;
        let width = (ui.available_width() - 32.0).clamp(290.0, 585.0);
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width);
                    workspace::mark(ui, 40.0);
                    ui.add_space(6.0);
                    ui.label(workspace::title("Shortcut settings", 26.0));
                    ui.label(workspace::muted(
                        "Choose which keys control windows on the remote desktop.",
                    ));
                    ui.add_space(12.0);
                    workspace::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (index, (name, _)) in ACTIONS.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(workspace::title(*name, 13.0));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                self.0[index].is_some(),
                                                egui::Button::new("Clear").frame(false),
                                            )
                                            .clicked()
                                        {
                                            self.0[index] = None;
                                            *recording = None;
                                        }
                                        let active = *recording == Some(index);
                                        let label = if active {
                                            "Press keys…".to_owned()
                                        } else {
                                            self.0[index].map_or_else(
                                                || "Add shortcut".to_owned(),
                                                Binding::name,
                                            )
                                        };
                                        if ui
                                            .button(label)
                                            .on_hover_text("Record a shortcut")
                                            .clicked()
                                        {
                                            *recording = if active { None } else { Some(index) };
                                        }
                                    },
                                );
                            });
                            if index < ACTIONS.len() - 1 {
                                ui.separator();
                            }
                        }
                        ui.add_space(6.0);
                        egui::CollapsingHeader::new("Choose keys manually")
                            .show(ui, |ui| self.edit_keys(ui));
                    });
                    ui.add_space(8.0);
                    let conflict = self.conflict();
                    if let Some(message) = &conflict {
                        ui.colored_label(ui.visuals().error_fg_color, message);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(conflict.is_none(), workspace::primary("Save shortcuts"))
                            .clicked()
                        {
                            result = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            result = Some(false);
                        }
                        if ui.button("Restore defaults").clicked() {
                            *self = Self::default();
                            *recording = None;
                        }
                    });
                },
            );
        });
        result
    }

    fn edit_keys(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("remote-shortcut-bindings")
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                for (index, (name, _)) in ACTIONS.iter().enumerate() {
                    ui.label(*name);
                    let mut binding =
                        self.0[index].unwrap_or(Binding::new(Key::Tab, Modifiers::NONE));
                    let mut key = self.0[index].map(|binding| binding.key);
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt(index)
                            .selected_text(key.map_or("Unassigned", Key::name))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut key, None, "Unassigned");
                                for key_option in
                                    Key::ALL.iter().copied().filter(|key| selectable(*key))
                                {
                                    ui.selectable_value(
                                        &mut key,
                                        Some(key_option),
                                        key_option.name(),
                                    );
                                }
                            });
                        if key.is_some() {
                            ui.checkbox(&mut binding.ctrl, "Ctrl");
                            ui.checkbox(&mut binding.alt, "Alt");
                            ui.checkbox(&mut binding.shift, "Shift");
                            #[cfg(target_os = "macos")]
                            ui.checkbox(&mut binding.cmd, "Cmd");
                        }
                    });
                    self.0[index] = key.map(|key| Binding { key, ..binding });
                    ui.end_row();
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Storage(HashMap<String, String>);

    impl eframe::Storage for Storage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.into(), value);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn saved_binding_replaces_default_and_unassigned_stays_unassigned() {
        let defaults = Bindings::default();
        assert_eq!(
            defaults.action(Key::Tab, Modifiers::CTRL | Modifiers::SHIFT),
            Some(WindowShortcut::NextApp)
        );
        let changed = Bindings([Some(Binding::new(Key::F8, Modifiers::ALT)), None, None]);
        let mut storage = Storage::default();
        changed.save(&mut storage);
        let loaded = Bindings::load(&storage);
        assert_eq!(
            loaded.action(Key::F8, Modifiers::ALT),
            Some(WindowShortcut::NextApp)
        );
        assert_eq!(
            loaded.action(Key::Tab, Modifiers::CTRL | Modifiers::SHIFT),
            None
        );
        assert_eq!(
            loaded.action(Key::Backspace, Modifiers::CTRL | Modifiers::SHIFT),
            None
        );
    }

    #[test]
    fn aliases_and_extra_modifiers_are_handled() {
        let bindings = Bindings([Some(Binding::new(Key::F8, Modifiers::CTRL)), None, None]);
        assert_eq!(
            bindings.action(Key::F8, Modifiers::CTRL | Modifiers::COMMAND),
            Some(WindowShortcut::NextApp)
        );
        assert_eq!(
            bindings.action(Key::F8, Modifiers::CTRL | Modifiers::SHIFT),
            None
        );
    }

    #[test]
    fn duplicate_and_clipboard_bindings_conflict() {
        let key = Some(Binding::new(Key::F8, Modifiers::ALT));
        assert!(Bindings([key, key, None])
            .conflict()
            .unwrap()
            .contains("both"));
        let clipboard_modifier = if cfg!(target_os = "macos") {
            Modifiers::MAC_CMD
        } else {
            Modifiers::CTRL
        };
        let key = Some(Binding::new(Key::C, clipboard_modifier | Modifiers::SHIFT));
        assert!(Bindings([key, None, None])
            .conflict()
            .unwrap()
            .contains("clipboard"));
    }
}
