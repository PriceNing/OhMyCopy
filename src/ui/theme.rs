use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

#[derive(Clone)]
pub struct GlassTheme {
    pub bg: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub warning: Color32,
}

impl GlassTheme {
    pub fn dark() -> Self {
        Self {
            bg: Color32::from_rgba_unmultiplied(20, 22, 28, 255),
            card: Color32::from_rgba_unmultiplied(40, 44, 56, 200),
            border: Color32::from_rgba_unmultiplied(180, 200, 255, 40),
            text: Color32::from_rgb(230, 234, 242),
            text_muted: Color32::from_rgb(140, 148, 165),
            accent: Color32::from_rgb(100, 180, 255),
            warning: Color32::from_rgb(255, 180, 80),
        }
    }
}

pub fn apply(ctx: &egui::Context, t: &GlassTheme) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = t.bg;
    visuals.window_fill = t.card;
    visuals.extreme_bg_color = t.bg;
    visuals.widgets.noninteractive.bg_fill = t.card;
    visuals.widgets.inactive.bg_fill = t.card;
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(60, 68, 90, 220);
    visuals.widgets.active.bg_fill = Color32::from_rgba_unmultiplied(70, 90, 130, 230);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
    visuals.selection.bg_fill = t.accent.gamma_multiply(0.35);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    visuals.widgets.active.corner_radius = CornerRadius::same(8);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    ctx.set_style(style);
}
