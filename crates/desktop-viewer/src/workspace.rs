use eframe::egui::{self, Color32, Stroke, Vec2};
use serde::{Deserialize, Serialize};

const RECENT_KEY: &str = "remote-desktop-viewer.recent-hosts";

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct Address {
    pub host: String,
    pub port: u16,
}

#[derive(Default)]
pub struct RecentHosts(Vec<Address>);

impl RecentHosts {
    pub fn load(storage: &dyn eframe::Storage) -> Self {
        let mut entries: Vec<Address> = eframe::get_value(storage, RECENT_KEY).unwrap_or_default();
        entries.retain(|entry| !entry.host.trim().is_empty() && entry.port != 0);
        entries.truncate(8);
        Self(entries)
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, RECENT_KEY, &self.0);
    }

    pub fn remember(&mut self, host: &str, port: u16) {
        let host = host.trim();
        self.0
            .retain(|entry| entry.port != port || !entry.host.eq_ignore_ascii_case(host));
        self.0.insert(
            0,
            Address {
                host: host.to_owned(),
                port,
            },
        );
        self.0.truncate(8);
    }

    pub fn entries(&self) -> &[Address] {
        &self.0
    }

    pub fn forget(&mut self, index: usize) {
        self.0.remove(index);
    }
}

pub const SHELL: Color32 = Color32::from_rgb(22, 27, 33);
pub const SIDEBAR: Color32 = Color32::from_rgb(18, 23, 29);
pub const CANVAS: Color32 = Color32::from_rgb(12, 16, 20);
pub const SURFACE: Color32 = Color32::from_rgb(26, 32, 39);
pub const BORDER: Color32 = Color32::from_rgb(47, 57, 67);
pub const TEXT: Color32 = Color32::from_rgb(238, 242, 247);
pub const MUTED: Color32 = Color32::from_rgb(155, 169, 182);
pub const ACCENT: Color32 = Color32::from_rgb(129, 226, 194);

fn semibold() -> egui::FontFamily {
    egui::FontFamily::Name("Inter semibold".into())
}

pub fn title(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text)
        .family(semibold())
        .size(size)
        .color(TEXT)
}

pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(12.5).color(MUTED)
}

pub fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(12.0)
        .inner_margin(egui::Margin::same(16.0))
}

pub fn field(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text)
        .min_size(Vec2::new(0.0, 38.0))
        .margin(egui::Margin::symmetric(10.0, 8.0))
        .vertical_align(egui::Align::Center)
}

pub fn primary(text: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(text).family(semibold()).color(CANVAS))
        .fill(ACCENT)
        .rounding(8.0)
        .min_size(Vec2::new(130.0, 38.0))
}

pub fn secondary(text: &str) -> egui::Button<'_> {
    egui::Button::new(text)
        .rounding(8.0)
        .min_size(Vec2::new(130.0, 38.0))
}

pub fn nav(text: &str, selected: bool) -> egui::Button<'_> {
    egui::Button::new(
        egui::RichText::new(text)
            .family(semibold())
            .color(if selected { ACCENT } else { TEXT }),
    )
    .rounding(8.0)
    .min_size(Vec2::new(102.0, 34.0))
}

pub fn appearance(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "Inter semibold".into(),
        egui::FontData::from_static(include_bytes!("../assets/Inter-SemiBold.ttf")),
    );
    let fallback = fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap();
    fallback.insert(0, "Inter".into());
    let mut bold = fallback.clone();
    bold.insert(0, "Inter semibold".into());
    fonts.families.insert(semibold(), bold);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(22.0, semibold()),
    );
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.window_margin = egui::Margin::same(16.0);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.faint_bg_color = SHELL;
    visuals.extreme_bg_color = SIDEBAR;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgb(43, 79, 73);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.window_rounding = 12.0.into();
    visuals.hyperlink_color = ACCENT;
    for (widget, color) in [
        (&mut visuals.widgets.inactive, SURFACE),
        (&mut visuals.widgets.hovered, Color32::from_rgb(41, 52, 61)),
        (&mut visuals.widgets.active, Color32::from_rgb(47, 65, 70)),
        (&mut visuals.widgets.open, SHELL),
    ] {
        widget.bg_fill = color;
        widget.weak_bg_fill = color;
        widget.bg_stroke = Stroke::new(1.0_f32, BORDER);
        widget.rounding = 8.0.into();
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals = visuals;
    ctx.set_style_of(egui::Theme::Dark, style);
}

#[derive(Clone, Copy)]
pub enum Icon {
    Monitor,
    Sidebar,
    Plus,
    Close,
}

pub fn icon(ui: &mut egui::Ui, icon: Icon, description: &str, selected: bool) -> egui::Response {
    let response = ui.add(
        egui::Button::new("")
            .frame(false)
            .min_size(Vec2::splat(34.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), description)
    });
    if selected || response.hovered() || response.has_focus() {
        ui.painter().rect_filled(
            response.rect,
            8.0,
            if selected {
                Color32::from_rgb(35, 65, 60)
            } else {
                SURFACE
            },
        );
    }
    draw_icon(
        ui,
        response.rect.center(),
        icon,
        if selected { ACCENT } else { TEXT },
    );
    response.on_hover_text(description)
}

pub fn mark(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 9.0, Color32::from_rgb(35, 65, 60));
    draw_icon(ui, rect.center(), Icon::Monitor, ACCENT);
}

pub fn status(ui: &mut egui::Ui, label: &str, live: bool) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 3.0, if live { ACCENT } else { MUTED });
        ui.label(muted(label));
    });
}

fn draw_icon(ui: &egui::Ui, center: egui::Pos2, icon: Icon, color: Color32) {
    let painter = ui.painter();
    let p = |x, y| center + Vec2::new(x, y);
    let stroke = Stroke::new(1.5_f32, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Monitor => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-8.0, -6.0), p(8.0, 4.5)),
                2.0,
                stroke,
            );
            line(&[(0.0, 4.5), (0.0, 7.0)]);
            line(&[(-4.0, 7.0), (4.0, 7.0)]);
        }
        Icon::Sidebar => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-8.0, -7.0), p(8.0, 7.0)),
                2.0,
                stroke,
            );
            line(&[(-2.5, -7.0), (-2.5, 7.0)]);
        }
        Icon::Plus => {
            line(&[(-6.0, 0.0), (6.0, 0.0)]);
            line(&[(0.0, -6.0), (0.0, 6.0)]);
        }
        Icon::Close => {
            line(&[(-5.0, -5.0), (5.0, 5.0)]);
            line(&[(-5.0, 5.0), (5.0, -5.0)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_vertically_centered_in_connection_fields() {
        let context = egui::Context::default();
        appearance(&context);
        let mut text = "minisforum".to_owned();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let output = field(&mut text).show(ui);
                let text_center = output.galley_pos.y + output.galley.size().y / 2.0;
                assert!((text_center - output.response.rect.center().y).abs() <= 1.0);
            });
        });
    }

    #[test]
    fn toolbar_menus_and_actions_share_the_same_size() {
        let context = egui::Context::default();
        appearance(&context);
        let mut sizes = Vec::new();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.horizontal(|ui| {
                    for label in ["Connection", "View"] {
                        sizes.push(
                            egui::menu::menu_custom_button(ui, nav(label, false), |_| ())
                                .response
                                .rect
                                .size(),
                        );
                    }
                    for label in ["Diagnostics", "Shortcuts", "Setup"] {
                        sizes.push(ui.add(nav(label, false)).rect.size());
                    }
                });
            });
        });
        assert_eq!(sizes, vec![Vec2::new(102.0, 34.0); 5]);
    }
}
